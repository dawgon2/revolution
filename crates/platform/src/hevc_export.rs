//! H.265 / HEVC export (Linux): VA-API hardware encoding in process ([`crate::va_enc`], fork
//! Phase 5.2b), else through the system ffmpeg: VA-API (`hevc_vaapi`), else libx265.
//!
//! In process, FilmCraft writes the parameter sets and slice headers and the driver the slice
//! data; nothing is piped. When it fails mid-stream, the frames it had not finished go to ffmpeg
//! (`hevc_vaapi`, else libx265 without B-frames), which carries on from an IDR picture.
//!
//! Through ffmpeg:
//! Rendered frames go to ffmpeg's stdin as raw `yuv420p` / `yuv420p10le` (converted by
//! [`filmcraft_export::frame_to_yuv420p`], the same matrix and range as the H.264 export) and the
//! encoded stream comes back on stdout as Annex B. FilmCraft splits it into access units, writes
//! them length-prefixed into its own MP4 (with AAC audio, metadata and colour boxes like every
//! other export), and derives each picture's presentation time from its picture order count, so
//! B-frames (libx265) get correct composition offsets.
//!
//! The `hvcC` record must be known when the muxer opens, before libx265's lookahead has output
//! anything, so [`HevcEncoder::new`] first encodes one black frame with exactly the same settings
//! and takes the parameter sets from it. That probe also checks the GPU takes the size and
//! profile: in Auto mode a VA-API failure there switches to libx265. Parameter sets in the real
//! stream that differ from the probe's stay in-band in their access unit.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, OnceLock};

use filmcraft_export::{ColorSignal, EncodedPacket, EncoderFrame, ExportError, ExportSettings, Format, HevcEncoder as Choice, Result, VideoEncoder};
use filmcraft_hevc::params::{Pps, Sps};
use filmcraft_hevc::slice::{NalHeader, SliceHeader};
use filmcraft_isobmff::{HevcConfig, HevcNalArray, SampleEntry};
use filmcraft_time::FrameRate;

use crate::ffmpeg::{StderrTail, annexb_nals, ffmpeg, vaapi_device, vaapi_supports};
use crate::va_enc::{EncodeParams, RateControl, VaHevcEncoder};

/// Raw frames queued for ffmpeg's stdin: enough for the export job's next batch to render while
/// ffmpeg encodes this one (fork Phase 5.2), within [`WRITE_QUEUE_BYTES`] (a 4K 10-bit frame is
/// 25 MB, 6.2K 78 MB), at least 3.
const WRITE_QUEUE_MAX: usize = 16;
const WRITE_QUEUE_BYTES: usize = 512 << 20;

fn write_queue(w: u32, h: u32, ten_bit: bool) -> usize {
    let frame = (w as usize * h as usize * 3 / 2).saturating_mul(if ten_bit { 2 } else { 1 }).max(1);
    (WRITE_QUEUE_BYTES / frame).clamp(3, WRITE_QUEUE_MAX)
}
/// Largest frame size taken.
const MAX_DIM: u32 = 8192;

/// Which ffmpeg HEVC encoders this ffmpeg has (`ffmpeg -encoders`). Probed once.
fn encoders() -> (bool, bool) {
    static E: OnceLock<(bool, bool)> = OnceLock::new();
    *E.get_or_init(|| {
        let Some(ff) = ffmpeg() else { return (false, false) };
        let out = Command::new(ff).args(["-hide_banner", "-encoders"]).stdin(Stdio::null()).stderr(Stdio::null()).output();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let has = |name: &str| text.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
        (has("hevc_vaapi"), has("libx265"))
    })
}

/// Whether the in-process VA-API encoder can be used (`FILMCRAFT_NO_VA_ENCODE` turns it off).
fn native(ten_bit: bool) -> bool {
    std::env::var_os("FILMCRAFT_NO_VA_ENCODE").is_none() && VaHevcEncoder::supported(ten_bit)
}

/// Whether HEVC export can run here (the driver's VA-API encoder, ffmpeg with `hevc_vaapi` + a
/// VA-API device, or `libx265`).
pub fn available() -> bool {
    let (vaapi, x265) = encoders();
    native(false) || native(true) || x265 || (vaapi && vaapi_device().is_some())
}

/// Register the encoder and mark [`Format::Hevc`] exportable (once; when [`available`]).
pub fn register() -> bool {
    static R: OnceLock<bool> = OnceLock::new();
    *R.get_or_init(|| {
        if !available() {
            log::info!("HEVC export unavailable: no VA-API encoder, and ffmpeg has neither hevc_vaapi (with a VA-API device) nor libx265");
            return false;
        }
        filmcraft_export::register_encoder(factory);
        filmcraft_export::register_format(Format::Hevc);
        true
    })
}

/// The encoder factory for [`Format::Hevc`].
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    (format == Format::Hevc).then(|| open_encoder(w, h, rate, s))
}

const VENDOR_NVIDIA: u32 = 0x10de;

/// Where NVENC goes among the H.265 encoders (fork XPS b).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NvencPlace {
    /// Before VA-API: the export renders on an NVIDIA GPU, whose own encoder saves a copy to
    /// another GPU.
    First,
    /// After the VA-API encoders, before libx265 (a hardware encoder is many times faster).
    BeforeX265,
    Never,
}

/// Auto only (Hardware means VA-API, Software x265); `render_vendor`: the PCI vendor of the GPU
/// exports render on. Whether NVENC has an H.265 encoder (`nvenc`, which loads CUDA and wakes a
/// sleeping NVIDIA GPU) is asked only to put it first; behind VA-API it is asked when VA-API has
/// failed (XPS 16, Power saving: every H.265 export woke the RTX 4050 for a probe it didn't need).
fn nvenc_place(choice: Choice, render_vendor: Option<u32>, nvenc: impl FnOnce() -> bool) -> NvencPlace {
    if choice != Choice::Auto {
        NvencPlace::Never
    } else if render_vendor != Some(VENDOR_NVIDIA) {
        NvencPlace::BeforeX265
    } else if nvenc() {
        NvencPlace::First
    } else {
        NvencPlace::Never
    }
}

/// The H.265 encoder for an export: VA-API in process, ffmpeg `hevc_vaapi`, NVENC and libx265 in
/// the order [`nvenc_place`] gives.
fn open_encoder(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<Box<dyn VideoEncoder>> {
    let boxed = |e: HevcEncoder| Box::new(e) as Box<dyn VideoEncoder>;
    #[cfg(target_pointer_width = "64")]
    {
        let place = nvenc_place(s.hevc_encoder, filmcraft_gpu::adapter_pick::chosen_vendor(), crate::nvenc::hevc_available);
        let nvenc = || crate::nvenc::export::linux_hevc_encoder(w, h, rate, s).inspect_err(|why| log::info!("HEVC export: NVENC not used: {why}"));
        match place {
            NvencPlace::First => {
                // fork 8.6d: NVENC failing part-way hands the export to VA-API in process (its
                // parameter sets in band), not an error
                let settings = s.clone();
                let then: crate::nvenc::fallback::MakeContinuation = Box::new(move || {
                    let target = LaneTarget { node: None, stream: None, in_band: true };
                    HevcEncoder::for_lane(w, h, rate, &settings, target).map(boxed)
                });
                // diagnostic: `FILMCRAFT_NVENC_NO_FALLBACK=1` ends the export when NVENC fails, as before 8.6d
                let first = if std::env::var_os("FILMCRAFT_NVENC_NO_FALLBACK").is_some_and(|v| !v.is_empty() && v != "0") {
                    nvenc()
                } else {
                    crate::nvenc::export::linux_hevc_encoder_with_fallback(w, h, rate, s, then)
                        .inspect_err(|why| log::info!("HEVC export: NVENC not used: {why}"))
                };
                if let Ok(e) = first {
                    log::info!("HEVC export: NVENC {w}x{h} (the export's GPU; VA-API carries on if it fails)");
                    return Ok(e);
                }
            }
            NvencPlace::BeforeX265 => {
                let hardware = HevcEncoder::new_with(w, h, rate, s, false);
                if let Ok(e) = hardware {
                    return Ok(boxed(e));
                }
                if crate::nvenc::hevc_available()
                    && let Ok(e) = nvenc()
                {
                    log::info!("HEVC export: NVENC {w}x{h}");
                    return Ok(e);
                }
            }
            NvencPlace::Never => {}
        }
    }
    HevcEncoder::new(w, h, rate, s).map(boxed)
}

/// The encoders, in order of preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The driver's encoder through libva, in process.
    VaNative,
    /// ffmpeg's `hevc_vaapi`.
    Vaapi,
    /// ffmpeg's `libx265`.
    X265,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::VaNative => "VA-API (in process)",
            Backend::Vaapi => "hevc_vaapi",
            Backend::X265 => "libx265",
        }
    }
}

/// Everything that decides ffmpeg's arguments.
#[derive(Clone, Debug)]
struct Params {
    w: u32,
    h: u32,
    rate: FrameRate,
    ten_bit: bool,
    keyint: u32,
    crf: Option<u8>,
    cbr: bool,
    target_kbps: u32,
    max_kbps: u32,
    signal: ColorSignal,
    /// No B-frames (libx265 continuing a stream started without them).
    no_b: bool,
    /// libx265's speed preset.
    x265_preset: &'static str,
    /// The hardware encoder's speed / quality level (fork 6E.2).
    hw_speed: filmcraft_export::HwEncodeSpeed,
    /// Main 4:2:2 10 (fork 6E.4; `ten_bit` is then set too): frames are `yuv422p10le`.
    chroma_422: bool,
    /// A lane of a two-GPU export (fork 6E.3): the in-process encoder only, on this GPU, writing
    /// this stream, and no ffmpeg fallback (it would write other parameter sets).
    lane: Option<LaneTarget>,
}

/// Where a lane's encoder runs and what it writes (fork 6E.3).
#[derive(Clone, Debug, Default)]
pub struct LaneTarget {
    /// Render node (`None`: the process's encode device).
    pub node: Option<String>,
    /// The stream all lanes share ([`HevcEncoder::lane_stream`]); `None`: the driver's own.
    pub stream: Option<filmcraft_hevc::write::StreamParams>,
    /// Keep the parameter sets in every IDR access unit and describe the file as `hev1`: for
    /// lanes whose drivers can't share one stream (`lane_stream` is `None`, fork 6E.3 S8).
    pub in_band: bool,
}

impl LaneTarget {
    fn display(&self) -> std::result::Result<Arc<crate::va::Display>, String> {
        match &self.node {
            Some(n) => crate::va::Display::for_node(n),
            None => crate::va::Display::for_encode(),
        }
    }
}

impl Params {
    fn va(&self) -> EncodeParams {
        let rc = match self.crf {
            Some(q) => RateControl::Cqp(q.clamp(1, 51)),
            None if self.cbr => RateControl::Cbr { kbps: self.target_kbps.max(100) },
            None => RateControl::Vbr { kbps: self.target_kbps.max(100), max_kbps: self.max_kbps.max(self.target_kbps).max(100) },
        };
        EncodeParams {
            width: self.w,
            height: self.h,
            rate_num: self.rate.num as u32,
            rate_den: self.rate.den as u32,
            ten_bit: self.ten_bit,
            keyint: self.keyint.max(1),
            rc,
            colour: (self.signal.primaries, self.signal.transfer, self.signal.matrix),
            speed: self.hw_speed,
            chroma_422: self.chroma_422,
            // a lane of a two-GPU export takes Intel's low-power encoder where it works (6E.3 S12)
            low_power: self.lane.is_some(),
        }
    }

    /// One black frame in the input format.
    fn black(&self) -> Vec<u8> {
        if self.ten_bit {
            let ch = if self.chroma_422 { self.h as usize } else { (self.h as usize).div_ceil(2) };
            let (y, c) = (self.w as usize * self.h as usize, 2 * (self.w as usize).div_ceil(2) * ch);
            let mut v = Vec::with_capacity(self.frame_bytes());
            v.extend(std::iter::repeat_n(64u16.to_le_bytes(), y).flatten());
            v.extend(std::iter::repeat_n(512u16.to_le_bytes(), c).flatten());
            v
        } else {
            let y = self.w as usize * self.h as usize;
            let mut v = vec![16u8; y];
            v.resize(self.frame_bytes(), 128);
            v
        }
    }

    fn frame_bytes(&self) -> usize {
        let (w, h) = (self.w as usize, self.h as usize);
        let ch = if self.chroma_422 { h } else { h.div_ceil(2) };
        (w * h + 2 * w.div_ceil(2) * ch) * if self.ten_bit { 2 } else { 1 }
    }

    /// ffmpeg's command line for `backend`, reading raw frames on stdin, writing Annex B on stdout.
    fn args(&self, backend: Backend, device: Option<&str>) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        let mut push = |xs: &[&str]| a.extend(xs.iter().map(|s| s.to_string()));
        push(&["-hide_banner", "-nostats", "-loglevel", "error"]);
        if let (Backend::Vaapi, Some(dev)) = (backend, device) {
            push(&["-vaapi_device", dev]);
        }
        let (pix, hw) = match (self.chroma_422, self.ten_bit) {
            (true, _) => ("yuv422p10le", "y210"),
            (false, true) => ("yuv420p10le", "p010"),
            (false, false) => ("yuv420p", "nv12"),
        };
        let size = format!("{}x{}", self.w, self.h);
        let fps = format!("{}/{}", self.rate.num.max(1), self.rate.den.max(1));
        push(&["-f", "rawvideo", "-pix_fmt", pix, "-s", &size, "-framerate", &fps, "-i", "pipe:0", "-an", "-sn", "-dn"]);
        // tagged on the frames (setparams) so the encoders write the VUI colour description
        let (p, t, m) = color_names(self.signal);
        let tag = format!("setparams=color_primaries={p}:color_trc={t}:colorspace={m}:range=tv");
        let profile = match (self.chroma_422, self.ten_bit) {
            (true, _) => "rext",
            (false, true) => "main10",
            (false, false) => "main",
        };
        let keyint = self.keyint.max(1).to_string();
        let (tk, mk) = (format!("{}k", self.target_kbps.max(100)), format!("{}k", self.max_kbps.max(self.target_kbps).max(100)));
        let bufsize = format!("{}k", self.max_kbps.max(self.target_kbps).max(100).saturating_mul(2));
        match backend {
            // (never asked for VaNative: it runs in process)
            Backend::Vaapi | Backend::VaNative => {
                let vf = format!("{tag},format={hw},hwupload");
                push(&["-vf", &vf, "-c:v", "hevc_vaapi", "-profile:v", profile, "-g", &keyint, "-idr_interval", "0"]);
                // the driver's quality level; ffmpeg clamps a level above the driver's range
                if let Some(level) = self.hw_speed.level(VaHevcEncoder::quality_range(self.ten_bit).unwrap_or(7)) {
                    push(&["-compression_level", &level.to_string()]);
                }
                // continuing a stream without reordering (after the in-process encoder failed):
                // no B-frames (AMD's encoder makes none anyway, Intel's does)
                if self.no_b {
                    push(&["-bf", "0"]);
                }
                match self.crf {
                    Some(q) => {
                        let q = q.clamp(1, 51).to_string();
                        push(&["-rc_mode", "CQP", "-qp", &q]);
                    }
                    None if self.cbr => push(&["-rc_mode", "CBR", "-b:v", &tk, "-maxrate", &tk, "-bufsize", &bufsize]),
                    None => push(&["-rc_mode", "VBR", "-b:v", &tk, "-maxrate", &mk, "-bufsize", &bufsize]),
                }
            }
            Backend::X265 => {
                let bframes = if self.no_b { ":bframes=0" } else { "" };
                let x265 = format!("log-level=error:keyint={keyint}:min-keyint={keyint}:open-gop=0:repeat-headers=1{bframes}");
                let profile = if self.chroma_422 { "main422-10" } else { profile };
                push(&["-vf", &tag, "-c:v", "libx265", "-preset", self.x265_preset, "-profile:v", profile, "-pix_fmt", pix, "-x265-params", &x265]);
                match self.crf {
                    Some(q) => {
                        let q = q.min(51).to_string();
                        push(&["-crf", &q]);
                    }
                    None if self.cbr => push(&["-b:v", &tk, "-maxrate", &tk, "-bufsize", &bufsize]),
                    None => push(&["-b:v", &tk, "-maxrate", &mk, "-bufsize", &bufsize]),
                }
            }
        }
        push(&["-f", "hevc", "pipe:1"]);
        a
    }
}

/// ffmpeg's names for the H.273 code points of `signal` (primaries, transfer, matrix).
fn color_names(signal: ColorSignal) -> (&'static str, &'static str, &'static str) {
    let primaries = if signal.primaries == 9 { "bt2020" } else { "bt709" };
    let transfer = match signal.transfer {
        16 => "smpte2084",
        18 => "arib-std-b67",
        _ => "bt709",
    };
    let matrix = if signal.matrix == 9 { "bt2020nc" } else { "bt709" };
    (primaries, transfer, matrix)
}

/// An ffmpeg encoding process.
struct Proc {
    child: Child,
    /// To the stdin writer (dropped to close stdin).
    tx: Option<SyncSender<Vec<u8>>>,
    /// Encoded bytes from the stdout reader; `Err` when stdout broke.
    rx: Receiver<std::result::Result<Vec<u8>, String>>,
    stderr: StderrTail,
}

impl Drop for Proc {
    fn drop(&mut self) {
        self.tx = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn thread(name: &str, f: impl FnOnce() + Send + 'static) -> std::result::Result<(), String> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
                log::error!("HEVC encoder thread panicked");
            }
        })
        .map(|_| ())
        .map_err(|e| format!("cannot start an encoder thread: {e}"))
}

impl Proc {
    fn start(args: &[String], queue: usize) -> std::result::Result<Proc, String> {
        let ff = ffmpeg().ok_or("ffmpeg is not installed")?;
        let mut child = Command::new(ff)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run ffmpeg: {e}"))?;
        let (Some(mut stdin), Some(mut stdout), Some(err)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("ffmpeg pipes missing".into());
        };
        let stderr = StderrTail::collect(err);
        let (tx, wrx) = std::sync::mpsc::sync_channel::<Vec<u8>>(queue.max(1));
        let (otx, rx) = std::sync::mpsc::channel();
        let started = thread("hevc-write", move || {
            for frame in wrx {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
        })
        .and_then(|()| {
            thread("hevc-read", move || {
                let mut buf = vec![0u8; 1 << 16];
                loop {
                    match stdout.read(&mut buf) {
                        Ok(0) => return,
                        Ok(n) => {
                            if otx.send(Ok(buf.get(..n).unwrap_or_default().to_vec())).is_err() {
                                return;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            let _ = otx.send(Err(e.to_string()));
                            return;
                        }
                    }
                }
            })
        });
        let p = Proc { child, tx: Some(tx), rx, stderr };
        started?;
        Ok(p)
    }

    fn failure(&self, what: &str) -> String {
        match self.stderr.text() {
            t if t.is_empty() => what.to_string(),
            t => format!("{what}: {}", t.lines().last().unwrap_or_default()),
        }
    }

    fn send(&mut self, frame: Vec<u8>) -> std::result::Result<(), String> {
        let tx = self.tx.as_ref().ok_or("encoder input closed")?;
        tx.send(frame).map_err(|_| self.failure("ffmpeg stopped"))
    }

    /// Encoded bytes available now.
    fn take(&mut self) -> std::result::Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(Ok(b)) => out.extend(b),
                Ok(Err(e)) => return Err(self.failure(&e)),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(out),
            }
        }
    }

    /// Close stdin and read everything up to the end; checks the exit status.
    fn finish(&mut self) -> std::result::Result<Vec<u8>, String> {
        self.tx = None;
        let mut out = Vec::new();
        while let Ok(r) = self.rx.recv() {
            match r {
                Ok(b) => out.extend(b),
                Err(e) => return Err(self.failure(&e)),
            }
        }
        let status = self.child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(self.failure(&format!("ffmpeg exited with {status}")));
        }
        Ok(out)
    }
}

/// One access unit out of the parser.
#[derive(Debug)]
struct Au {
    /// Length-prefixed (4-byte) NAL units.
    data: Vec<u8>,
    key: bool,
    /// Output order index (from the picture order count).
    display: i64,
}

fn rbsp(nal: &[u8]) -> Vec<u8> {
    filmcraft_bitstream::unescape_rbsp(nal.get(2..).unwrap_or_default())
}

fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(63, |h| (h >> 1) & 0x3f)
}

/// Splits an Annex B HEVC stream into access units and numbers them in output order.
#[derive(Default)]
struct Parser {
    /// Bytes not split yet (from the last start code on).
    pending: Vec<u8>,
    /// NAL units of the access unit being collected.
    au: Vec<Vec<u8>>,
    au_has_vcl: bool,
    /// Parameter sets by id (raw NAL units) and parsed.
    vps: HashMap<u32, Vec<u8>>,
    sps: HashMap<u32, (Vec<u8>, Arc<Sps>)>,
    pps: HashMap<u32, (Vec<u8>, Arc<Pps>)>,
    /// Parameter sets that the sample entry carries (left out of the samples).
    configured: Vec<Vec<u8>>,
    /// Access units output so far (decode order index of the next one).
    decoded: i64,
    /// Output index of POC 0 of the current coded video sequence.
    base: i64,
    prev_tid0: (i64, i64),
    first: bool,
}

impl Parser {
    fn new() -> Self {
        Parser { first: true, ..Default::default() }
    }

    /// Feed encoder output; complete access units come out.
    fn feed(&mut self, bytes: &[u8]) -> std::result::Result<Vec<Au>, String> {
        self.pending.extend_from_slice(bytes);
        // everything up to the last start code is complete
        let Some(last) = self.pending.windows(3).rposition(|w| w == [0, 0, 1]) else { return Ok(Vec::new()) };
        if last == 0 {
            return Ok(Vec::new());
        }
        let cut = if last > 0 && self.pending.get(last - 1) == Some(&0) { last - 1 } else { last };
        let rest = self.pending.split_off(cut);
        let done = std::mem::replace(&mut self.pending, rest);
        self.nals(&done)
    }

    /// End of stream: the rest is the last NAL unit; the last access unit comes out.
    fn finish(&mut self) -> std::result::Result<Vec<Au>, String> {
        let done = std::mem::take(&mut self.pending);
        let mut out = self.nals(&done)?;
        if self.au_has_vcl {
            out.push(self.close_au()?);
        }
        Ok(out)
    }

    fn nals(&mut self, data: &[u8]) -> std::result::Result<Vec<Au>, String> {
        let mut out = Vec::new();
        for nal in annexb_nals(data) {
            let t = nal_type(nal);
            let first_slice = t < 32 && nal.get(2).is_some_and(|b| b & 0x80 != 0);
            let starts_au = matches!(t, 32..=35 | 39 | 41..=44 | 48..=55) || first_slice;
            if self.au_has_vcl && starts_au {
                out.push(self.close_au()?);
            }
            match t {
                32 => {
                    let id = rbsp(nal).first().map_or(0, |b| (b >> 4) as u32);
                    self.vps.insert(id, nal.to_vec());
                }
                33 => {
                    let sps = Sps::parse(&rbsp(nal)).map_err(|e| format!("encoder SPS: {e}"))?;
                    self.sps.insert(sps.id, (nal.to_vec(), Arc::new(sps)));
                }
                34 => {
                    let pps = Pps::parse(&rbsp(nal)).map_err(|e| format!("encoder PPS: {e}"))?;
                    self.pps.insert(pps.id, (nal.to_vec(), Arc::new(pps)));
                }
                _ => {}
            }
            if t < 32 {
                self.au_has_vcl = true;
            }
            self.au.push(nal.to_vec());
        }
        Ok(out)
    }

    /// The access unit collected so far, with its output index.
    fn close_au(&mut self) -> std::result::Result<Au, String> {
        let nals = std::mem::take(&mut self.au);
        self.au_has_vcl = false;
        let slice = nals.iter().find(|n| nal_type(n) < 32).ok_or("access unit without a picture")?;
        let header = NalHeader::parse(slice).map_err(|e| format!("encoder slice: {e}"))?;
        let lookup = |id: u32| {
            let (_, pps) = self.pps.get(&id).ok_or_else(|| filmcraft_hevc::Error::Invalid(format!("PPS {id} missing")))?;
            let (_, sps) = self.sps.get(&pps.sps_id).ok_or_else(|| filmcraft_hevc::Error::Invalid(format!("SPS {} missing", pps.sps_id)))?;
            Ok((pps.clone(), sps.clone()))
        };
        let (sh, _, sps) = SliceHeader::parse(&rbsp(slice), header, lookup, None).map_err(|e| format!("encoder slice header: {e}"))?;
        // picture order count (8.3.1)
        let max_lsb = 1i64 << sps.log2_max_poc_lsb.min(16);
        let lsb = sh.poc_lsb as i64;
        let no_rasl = header.is_idr() || header.is_bla() || (header.is_cra() && self.first);
        let msb = if no_rasl {
            0
        } else {
            let (plsb, pmsb) = self.prev_tid0;
            if lsb < plsb && plsb - lsb >= max_lsb / 2 {
                pmsb + max_lsb
            } else if lsb > plsb && lsb - plsb > max_lsb / 2 {
                pmsb - max_lsb
            } else {
                pmsb
            }
        };
        let poc = msb + lsb;
        if header.temporal_id == 0 && !header.is_rasl() && !header.is_radl() && !header.is_sub_layer_non_ref() {
            self.prev_tid0 = (lsb, msb);
        }
        if header.is_idr() || header.is_bla() || self.first {
            // a closed GOP starts here: every earlier picture is output before it
            self.base = self.decoded - poc;
        }
        self.first = false;
        let display = self.base + poc;
        self.decoded += 1;
        let mut data = Vec::new();
        for n in &nals {
            let t = nal_type(n);
            // access unit delimiters, and parameter sets the sample entry has, stay out
            if t == 35 || (matches!(t, 32..=34) && self.configured.iter().any(|c| c == n)) {
                continue;
            }
            data.extend_from_slice(&(n.len() as u32).to_be_bytes());
            data.extend_from_slice(n);
        }
        Ok(Au { data, key: header.is_irap(), display })
    }

    /// The `hvcC` record of the parameter sets seen (VPS, SPS, PPS).
    fn config(&self) -> std::result::Result<HevcConfig, String> {
        let (sps_nal, sps) = self.sps.values().next().ok_or("the encoder wrote no SPS")?;
        let vps: Vec<Vec<u8>> = self.vps.values().cloned().collect();
        let pps: Vec<Vec<u8>> = self.pps.values().map(|p| p.0.clone()).collect();
        if vps.is_empty() || pps.is_empty() {
            return Err("the encoder wrote no VPS or PPS".into());
        }
        // sps_video_parameter_set_id(4) max_sub_layers_minus1(3) temporal_id_nesting(1), then the
        // general profile_tier_level: 1 + 4 + 6 + 1 bytes
        let r = rbsp(sps_nal);
        let ptl = r.get(..13).ok_or("SPS too short")?;
        let constraint = ptl.get(6..12).map_or(0u64, |b| b.iter().fold(0u64, |a, &x| (a << 8) | x as u64));
        Ok(HevcConfig {
            general_profile_space: ptl[1] >> 6,
            general_tier_flag: ptl[1] & 0x20 != 0,
            general_profile_idc: ptl[1] & 0x1f,
            general_profile_compatibility_flags: u32::from_be_bytes([ptl[2], ptl[3], ptl[4], ptl[5]]),
            general_constraint_indicator_flags: constraint,
            general_level_idc: ptl[12],
            min_spatial_segmentation_idc: 0,
            parallelism_type: 0,
            chroma_format_idc: sps.chroma_format_idc as u8,
            bit_depth_luma: sps.bit_depth_luma as u8,
            bit_depth_chroma: sps.bit_depth_chroma as u8,
            avg_frame_rate: 0,
            constant_frame_rate: 0,
            num_temporal_layers: ((ptl[0] >> 1) & 7) + 1,
            temporal_id_nested: ptl[0] & 1 != 0,
            length_size: 4,
            arrays: vec![
                HevcNalArray { completeness: true, nal_type: 32, nalus: vps },
                HevcNalArray { completeness: true, nal_type: 33, nalus: vec![sps_nal.clone()] },
                HevcNalArray { completeness: true, nal_type: 34, nalus: pps },
            ],
        })
    }

    /// Reorder depth of the stream (highest sub-layer).
    fn reorder(&self) -> i64 {
        self.sps.values().next().map_or(0, |s| s.1.max_num_reorder as i64)
    }
}

/// Encode one black frame with `args` and return the parser that saw its parameter sets.
fn probe(args: &[String], p: &Params) -> std::result::Result<Parser, String> {
    let mut proc = Proc::start(args, 3)?;
    proc.send(p.black())?;
    let out = proc.finish()?;
    parse_probe(&out)
}

/// Encode one black frame in process and return the parser that saw its parameter sets.
fn probe_native(p: &Params) -> std::result::Result<Parser, String> {
    let mut enc = native_encoder(p)?;
    let mut out: Vec<u8> = enc.encode(&p.black())?.concat();
    out.extend(enc.flush()?.concat());
    parse_probe(&out)
}

/// The in-process encoder for `p`: on the lane's GPU with the lane's stream, else the default.
fn native_encoder(p: &Params) -> std::result::Result<VaHevcEncoder, String> {
    match &p.lane {
        Some(l) => VaHevcEncoder::new_on_with(p.va(), l.display()?, l.stream.clone()),
        None => VaHevcEncoder::new(p.va()),
    }
}

fn parse_probe(out: &[u8]) -> std::result::Result<Parser, String> {
    let mut parser = Parser::new();
    let aus = parser.feed(out)?.into_iter().chain(parser.finish()?).count();
    if aus != 1 {
        return Err(format!("probe encode gave {aus} pictures"));
    }
    Ok(parser)
}

/// Where the frames go.
enum Engine {
    Ffmpeg(Proc),
    Va(VaWorker),
}

/// The in-process encoder on its own thread, behind a queue like ffmpeg's stdin, so the export
/// job renders the next batch while it encodes this one.
/// A frame for the in-process encoder.
#[derive(Clone)]
enum Input {
    /// Planar 4:2:0 bytes, uploaded by the encoder.
    Bytes(Arc<Vec<u8>>),
    /// Rendered on the GPU into one of the encoder's input surfaces (fork Phase 5.2c).
    Gpu(Arc<dyn filmcraft_export::GpuPicture>),
}

impl Input {
    /// The frame as planar 4:2:0 bytes (a GPU frame is read back).
    fn into_bytes(self) -> std::result::Result<Vec<u8>, String> {
        match self {
            Input::Bytes(b) => Ok(Arc::try_unwrap(b).unwrap_or_else(|b| b.as_ref().clone())),
            Input::Gpu(p) => p.read_yuv420().ok_or_else(|| "a frame rendered on the GPU could not be read back".to_string()),
        }
    }
}

/// Encode one input: a frame in an encoder surface is encoded in place once the GPU has written it.
fn encode_input(enc: &mut VaHevcEncoder, f: &Input) -> std::result::Result<Vec<Vec<u8>>, String> {
    match f {
        Input::Bytes(b) => enc.encode(b),
        Input::Gpu(p) => match p.as_any().downcast_ref::<crate::gpu_export::VaPicture>() {
            Some(v) => {
                let t = filmcraft_export::trace::start();
                v.ready()?;
                filmcraft_export::trace::add(filmcraft_export::trace::ENC_GPU_WAIT, t);
                let t = filmcraft_export::trace::start();
                let r = enc.encode_surface(v.surface.clone());
                filmcraft_export::trace::add(filmcraft_export::trace::ENC_VA, t);
                r
            }
            None => enc.encode(&f.clone().into_bytes()?),
        },
    }
}

struct VaWorker {
    tx: Option<SyncSender<Input>>,
    /// Where frames may be rendered for the encoder (its input surfaces).
    target: Arc<crate::va_enc::EncodeTarget>,
    /// One access unit per frame, in order; `Err` when the encoder failed (it stops then).
    rx: Receiver<std::result::Result<Vec<u8>, String>>,
    /// Frames sent whose access unit has not come back (oldest first): ffmpeg encodes them again
    /// if the encoder fails.
    pending: std::collections::VecDeque<Input>,
    /// Frame buffers back from the encoder, for the next frames (no fresh allocation per frame).
    free: Vec<Vec<u8>>,
}

impl VaWorker {
    fn start(mut enc: VaHevcEncoder, queue: usize, _node: Option<&str>) -> std::result::Result<VaWorker, String> {
        let (tx, frames) = std::sync::mpsc::sync_channel::<Input>(queue.max(1));
        let target = Arc::new(enc.target());
        let (out, rx) = std::sync::mpsc::channel();
        #[cfg(test)]
        let fail_after = tests::take_fail_after(_node);
        thread("hevc-va", move || {
            let send = |r: std::result::Result<Vec<Vec<u8>>, String>| match r {
                Ok(aus) => aus.into_iter().all(|au| out.send(Ok(au)).is_ok()),
                Err(e) => {
                    let _ = out.send(Err(e));
                    false
                }
            };
            #[cfg(test)]
            let mut n = 0;
            for f in frames {
                #[cfg(test)]
                {
                    n += 1;
                    if n == fail_after {
                        send(Err("simulated failure".into()));
                        return;
                    }
                }
                if !send(encode_input(&mut enc, &f)) {
                    return;
                }
            }
            // input closed: the end of the stream
            send(enc.flush());
        })?;
        Ok(VaWorker { tx: Some(tx), target, rx, pending: Default::default(), free: Vec::new() })
    }

    fn send(&mut self, frame: Input) -> std::result::Result<(), String> {
        self.pending.push_back(frame.clone());
        let tx = self.tx.as_ref().ok_or("encoder input closed")?;
        let t = filmcraft_export::trace::start();
        let r = tx.send(frame).map_err(|_| "the encoder thread stopped".to_string());
        filmcraft_export::trace::add(filmcraft_export::trace::ENC_QUEUE, t);
        r
    }

    /// Access units ready (`end`: close the input and wait for all of them), and the encoder's
    /// error if it failed.
    fn take(&mut self, end: bool) -> (Vec<u8>, Option<String>) {
        if end {
            self.tx = None;
        }
        let mut bytes = Vec::new();
        loop {
            let r = if end { self.rx.recv().map_err(|_| TryRecvError::Disconnected) } else { self.rx.try_recv() };
            match r {
                Ok(Ok(au)) => {
                    // the encoder thread has dropped its reference once the picture is out
                    if let Some(Input::Bytes(f)) = self.pending.pop_front()
                        && let Ok(f) = Arc::try_unwrap(f)
                        && self.free.len() < 2
                    {
                        self.free.push(f);
                    }
                    bytes.extend(au);
                }
                Ok(Err(e)) => return (bytes, Some(e)),
                Err(TryRecvError::Empty) => return (bytes, None),
                Err(TryRecvError::Disconnected) => {
                    let lost = (!self.pending.is_empty()).then(|| "the encoder thread stopped".to_string());
                    return (bytes, lost);
                }
            }
        }
    }
}

/// The HEVC export encoder.
pub struct HevcEncoder {
    params: Params,
    backend: Backend,
    engine: Engine,
    parser: Parser,
    config: HevcConfig,
    reorder: i64,
    fed: i64,
    yuv: Vec<u8>,
}

impl HevcEncoder {
    pub fn new(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<Self> {
        Self::new_with(w, h, rate, s, true)
    }

    /// [`new`](Self::new); `x265`: whether Auto may end with libx265.
    fn new_with(w: u32, h: u32, rate: FrameRate, s: &ExportSettings, x265: bool) -> Result<Self> {
        let params = Self::params(w, h, rate, s)?;
        let (has_vaapi, has_x265) = encoders();
        let profile = if params.ten_bit { "VAProfileHEVCMain10" } else { "VAProfileHEVCMain" };
        let vaapi_ok = has_vaapi && vaapi_device().is_some() && vaapi_supports(profile, "VAEntrypointEncSlice");
        let native_ok = native(params.ten_bit);
        let va = [native_ok.then_some(Backend::VaNative), vaapi_ok.then_some(Backend::Vaapi)];
        let order: Vec<Backend> = match s.hevc_encoder {
            Choice::Auto => va.into_iter().chain([(x265 && has_x265).then_some(Backend::X265)]).flatten().collect(),
            Choice::Vaapi if native_ok || vaapi_ok => va.into_iter().flatten().collect(),
            Choice::Vaapi => {
                return Err(ExportError::Unsupported(
                    "VA-API HEVC encoding is not available (the driver's HEVC encoder, or ffmpeg hevc_vaapi with a VA-API device, is needed)".into(),
                ));
            }
            Choice::X265 if has_x265 => vec![Backend::X265],
            Choice::X265 => return Err(ExportError::Unsupported("this ffmpeg has no libx265".into())),
        };
        Self::with_backends(params, &order)
    }

    /// Checked parameters of an export.
    fn params(w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Result<Params> {
        if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            return Err(ExportError::Unsupported(format!("HEVC export of {w}x{h} (even sizes up to {MAX_DIM})")));
        }
        if rate.num <= 0 || rate.den <= 0 || rate.num > u32::MAX as i64 || rate.den > u32::MAX as i64 {
            return Err(ExportError::Unsupported(format!("frame rate {}/{}", rate.num, rate.den)));
        }
        Ok(Params {
            w,
            h,
            rate,
            ten_bit: s.hevc_bit_depth == 10 || s.hevc_chroma_422,
            keyint: s.keyframe_distance.unwrap_or(0).max(1),
            crf: (s.bitrate_mode == filmcraft_export::BitrateMode::Crf).then(|| s.crf.round().clamp(0.0, 51.0) as u8),
            cbr: s.bitrate_mode == filmcraft_export::BitrateMode::Cbr,
            target_kbps: s.bitrate_kbps,
            max_kbps: s.max_bitrate_kbps.unwrap_or(s.bitrate_kbps),
            signal: s.signal,
            no_b: false,
            x265_preset: filmcraft_export::X265_PRESETS.iter().find(|p| **p == s.x265_preset).copied().unwrap_or("medium"),
            hw_speed: s.hw_encode_speed,
            chroma_422: s.hevc_chroma_422,
            lane: None,
        })
    }

    /// An encoder for one lane of a two-GPU export (fork 6E.3): in process only, on `lane`'s GPU,
    /// writing its stream; a failure is an error (the chunk goes to another lane), never ffmpeg.
    pub fn for_lane(w: u32, h: u32, rate: FrameRate, s: &ExportSettings, lane: LaneTarget) -> Result<Self> {
        let mut params = Self::params(w, h, rate, s)?;
        if params.chroma_422 {
            return Err(ExportError::Unsupported("two-GPU export of 4:2:2".into()));
        }
        params.lane = Some(lane);
        Self::with_backends(params, &[Backend::VaNative])
    }

    /// The stream every one of `nodes` (`None`: the process's encode device) can write for this
    /// export, so their chunks share one set of parameter sets (fork 6E.3); `None` when one of
    /// them has no encoder for it or the drivers have nothing in common.
    pub fn lane_stream(w: u32, h: u32, rate: FrameRate, s: &ExportSettings, nodes: &[Option<String>]) -> Option<filmcraft_hevc::write::StreamParams> {
        let params = Self::params(w, h, rate, s).ok()?;
        // as the lanes will open them (`Params::va` with a lane): LP where usable (6E.3 S12)
        let va = EncodeParams { low_power: true, ..params.va() };
        let mut common: Option<filmcraft_hevc::write::StreamParams> = None;
        for node in nodes {
            let d = LaneTarget { node: node.clone(), ..LaneTarget::default() }.display().ok()?;
            // LP's stream within what EncSlice writes too, so a lane whose LP encoder fails to
            // open still accepts the shared stream on EncSlice (6E.3 S12 review)
            let slice = VaHevcEncoder::stream_params_for(&EncodeParams { low_power: false, ..va.clone() }, &d).ok()?;
            let lp = VaHevcEncoder::stream_params_for(&va, &d).ok()?;
            let own = filmcraft_hevc::write::common_stream(&lp, &slice).unwrap_or(slice);
            common = Some(match common {
                None => own,
                Some(c) => filmcraft_hevc::write::common_stream(&c, &own)?,
            });
        }
        common
    }

    /// The first of `order` that starts (tests force a backend with it).
    pub fn with(w: u32, h: u32, rate: FrameRate, s: &ExportSettings, order: &[Backend]) -> Result<Self> {
        Self::with_backends(Self::params(w, h, rate, s)?, order)
    }

    fn with_backends(params: Params, order: &[Backend]) -> Result<Self> {
        let (w, h) = (params.w, params.h);
        let mut why = Vec::new();
        for &backend in order {
            let started = match backend {
                Backend::VaNative => probe_native(&params).and_then(|probe| {
                    let config = probe.config()?;
                    let enc = native_encoder(&params)?;
                    let worker = VaWorker::start(enc, write_queue(params.w, params.h, params.ten_bit), params.lane.as_ref().and_then(|l| l.node.as_deref()))?;
                    Ok((probe, config, Engine::Va(worker)))
                }),
                _ => {
                    // the in-process encoder's device when one is set; 4:2:2 on a device that encodes
                    // it (fork 6E.4: Intel)
                    let encode_node = if params.chroma_422 {
                        VaHevcEncoder::node_422()
                    } else {
                        std::env::var("FILMCRAFT_VAAPI_ENCODE_DEVICE").ok().filter(|v| !v.trim().is_empty())
                    };
                    let args = params.args(backend, encode_node.as_deref().or(vaapi_device()));
                    probe(&args, &params).and_then(|probe| {
                        let config = probe.config()?;
                        let proc = Proc::start(&args, write_queue(params.w, params.h, params.ten_bit))?;
                        Ok((probe, config, Engine::Ffmpeg(proc)))
                    })
                }
            };
            match started {
                Ok((probe, config, engine)) => {
                    log::info!("HEVC export: {} {}x{} {}-bit", backend.name(), w, h, if params.ten_bit { 10 } else { 8 });
                    let mut parser = Parser::new();
                    if !params.lane.as_ref().is_some_and(|l| l.in_band) {
                        parser.configured = config.arrays.iter().flat_map(|a| a.nalus.iter().cloned()).collect();
                    }
                    let reorder = probe.reorder();
                    return Ok(HevcEncoder { params, backend, engine, parser, config, reorder, fed: 0, yuv: Vec::new() });
                }
                Err(e) => {
                    log::warn!("HEVC export: {} unusable: {e}", backend.name());
                    why.push(format!("{}: {e}", backend.name()));
                }
            }
        }
        if why.is_empty() {
            why.push("ffmpeg has no HEVC encoder".into());
        }
        Err(ExportError::Unsupported(format!("HEVC encoder not available ({})", why.join("; "))))
    }

    /// The encoder in use.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The in-process encoder failed (`why`): its unfinished frames, and every frame after them,
    /// go to ffmpeg (`hevc_vaapi`, else libx265 without B-frames), which starts with an IDR
    /// picture. The output order stays the decode order, as the stream so far has no reordering.
    fn fall_back(&mut self, why: String) -> Result<()> {
        if self.params.lane.is_some() {
            return Err(ExportError::Encode(why));
        }
        let Engine::Va(worker) = &mut self.engine else { return Err(ExportError::Encode(why)) };
        let pending: Vec<Vec<u8>> = std::mem::take(&mut worker.pending)
            .into_iter()
            .map(Input::into_bytes)
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| ExportError::Encode(format!("{why}; {e}")))?;
        let (has_vaapi, has_x265) = encoders();
        let profile = if self.params.ten_bit { "VAProfileHEVCMain10" } else { "VAProfileHEVCMain" };
        let vaapi_ok = has_vaapi && vaapi_device().is_some() && vaapi_supports(profile, "VAEntrypointEncSlice");
        let mut params = self.params.clone();
        params.no_b = true;
        let mut errors = vec![format!("{}: {why}", Backend::VaNative.name())];
        for backend in [vaapi_ok.then_some(Backend::Vaapi), has_x265.then_some(Backend::X265)].into_iter().flatten() {
            match Proc::start(&params.args(backend, vaapi_device()), write_queue(params.w, params.h, params.ten_bit)) {
                Ok(mut proc) => {
                    log::warn!("HEVC export: {why}; continuing with {} ({} frames again)", backend.name(), pending.len());
                    for f in pending {
                        proc.send(f).map_err(ExportError::Encode)?;
                    }
                    self.engine = Engine::Ffmpeg(proc);
                    self.backend = backend;
                    self.params = params;
                    return Ok(());
                }
                Err(e) => errors.push(format!("{}: {e}", backend.name())),
            }
        }
        Err(ExportError::Encode(format!("HEVC encoder failed ({})", errors.join("; "))))
    }

    /// Packets with composition offsets: decode index `d` gets dts `d - reorder` (frames), its
    /// pts is its output index.
    fn stamp(&mut self, aus: Vec<Au>, first_decode: i64) -> Result<Vec<EncodedPacket>> {
        let den = self.params.rate.den;
        let mut out = Vec::with_capacity(aus.len());
        for (k, au) in aus.into_iter().enumerate() {
            let d = first_decode + k as i64;
            let offset = au.display - (d - self.reorder);
            if offset < 0 || au.display >= self.fed || au.display < 0 {
                return Err(ExportError::Encode(format!("HEVC encoder output order is inconsistent (picture {d} shown at {})", au.display)));
            }
            let co = offset.checked_mul(den).and_then(|v| i32::try_from(v).ok()).ok_or_else(|| ExportError::Encode("composition offset overflow".into()))?;
            out.push(EncodedPacket { data: au.data, key: au.key, duration: den as u32, composition_offset: co });
        }
        Ok(out)
    }

    fn drain(&mut self, bytes: &[u8], end: bool) -> Result<Vec<EncodedPacket>> {
        let first = self.parser.decoded;
        let mut aus = self.parser.feed(bytes).map_err(ExportError::Encode)?;
        if end {
            aus.extend(self.parser.finish().map_err(ExportError::Encode)?);
        }
        self.stamp(aus, first)
    }
}

impl VideoEncoder for HevcEncoder {
    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = (self.params.w as u16, self.params.h as u16);
        let mut e = if self.params.lane.as_ref().is_some_and(|l| l.in_band) {
            SampleEntry::hevc_inband(self.config.clone(), w, h)
        } else {
            SampleEntry::hevc(self.config.clone(), w, h)
        };
        self.params.signal.apply_to(&mut e, false);
        e
    }

    fn timescale(&self) -> u32 {
        self.params.rate.num as u32
    }

    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        if (f.width, f.height) != (self.params.w, self.params.h) {
            return Err(ExportError::Encode(format!("frame is {}x{}, the encoder {}x{}", f.width, f.height, self.params.w, self.params.h)));
        }
        // rendered into the encoder's own input surface: encoded in place (fork Phase 5.2c)
        // (4:2:2 never is: its surfaces are Y210, which the compositor doesn't write)
        if !self.params.chroma_422
            && let (Engine::Va(worker), Some(gpu), None) = (&mut self.engine, &f.gpu, f.yuv)
        {
            self.fed += 1;
            let sent = worker.send(Input::Gpu(gpu.clone()));
            let (bytes, failed) = worker.take(false);
            if let Some(e) = failed.or(sent.err()) {
                self.fall_back(e)?;
            }
            return self.drain(&bytes, false);
        }
        let mut yuv = match &mut self.engine {
            Engine::Va(worker) => worker.free.pop().unwrap_or_default(),
            Engine::Ffmpeg(_) => std::mem::take(&mut self.yuv),
        };
        if self.params.chroma_422 {
            let clock = filmcraft_export::trace::start();
            filmcraft_export::frame_to_yuv422p10le(f, self.params.signal, &mut yuv);
            filmcraft_export::trace::add(filmcraft_export::trace::ENC_422_PREP, clock);
        } else {
            filmcraft_export::frame_to_yuv420p(f, self.params.signal, self.params.ten_bit, &mut yuv);
        }
        self.fed += 1;
        let bytes = match &mut self.engine {
            Engine::Ffmpeg(proc) => {
                let frame = yuv.clone();
                self.yuv = yuv;
                proc.send(frame).map_err(ExportError::Encode)?;
                proc.take().map_err(ExportError::Encode)?
            }
            Engine::Va(worker) => {
                let sent = worker.send(Input::Bytes(Arc::new(yuv)));
                let (bytes, failed) = worker.take(false);
                if let Some(e) = failed.or(sent.err()) {
                    self.fall_back(e)?;
                }
                bytes
            }
        };
        self.drain(&bytes, false)
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        let mut bytes = Vec::new();
        if let Engine::Va(worker) = &mut self.engine {
            let (b, failed) = worker.take(true);
            bytes = b;
            if let Some(e) = failed {
                self.fall_back(e)?;
            }
        }
        if let Engine::Ffmpeg(proc) = &mut self.engine {
            bytes.extend(proc.finish().map_err(ExportError::Encode)?);
        }
        let out = self.drain(&bytes, true)?;
        if self.parser.decoded != self.fed {
            return Err(ExportError::Encode(format!("HEVC encoder returned {} of {} pictures", self.parser.decoded, self.fed)));
        }
        Ok(out)
    }

    fn media_start(&self) -> Option<i64> {
        (self.reorder > 0).then(|| self.reorder.saturating_mul(self.params.rate.den))
    }

    fn gpu_target(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        match &self.engine {
            Engine::Va(worker) => Some(worker.target.clone()),
            Engine::Ffmpeg(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    /// Fork XPS b: NVENC comes first when the export renders on an NVIDIA GPU, after VA-API
    /// otherwise, and never for Hardware (VA-API) / Software (x265) or without an NVENC encoder.
    #[test]
    fn nvenc_takes_its_place_among_the_hevc_encoders() {
        use super::{NvencPlace as P, nvenc_place};
        assert_eq!(nvenc_place(Choice::Auto, Some(0x10de), || true), P::First);
        // behind VA-API the probe waits until VA-API has failed (it wakes a sleeping NVIDIA GPU)
        assert_eq!(nvenc_place(Choice::Auto, Some(0x8086), || panic!("no probe on Power saving")), P::BeforeX265);
        assert_eq!(nvenc_place(Choice::Auto, None, || panic!("no probe without a known GPU")), P::BeforeX265);
        assert_eq!(nvenc_place(Choice::Auto, Some(0x10de), || false), P::Never);
        for c in [Choice::Vaapi, Choice::X265] {
            assert_eq!(nvenc_place(c, Some(0x10de), || panic!("the probe isn't needed")), P::Never);
        }
    }

    use super::*;
    use filmcraft_frame::PixelData;

    /// The next in-process encoder started fails on this frame (1-based; 0: never).
    pub(super) static FAIL_AFTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// When set, only an encoder started on this render node takes [`FAIL_AFTER`] (one lane fails).
    static FAIL_NODE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    pub(super) fn take_fail_after(node: Option<&str>) -> u64 {
        let only = FAIL_NODE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match only.as_deref() {
            Some(n) if Some(n) != node => 0,
            _ => FAIL_AFTER.swap(0, std::sync::atomic::Ordering::SeqCst),
        }
    }

    /// Tests that start in-process encoders while another may set [`FAIL_AFTER`] run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Fork 6E.3: an encoder per GPU on the shared stream; each lane's chunk (a fresh encoder, so
    /// it starts with an IDR) decodes with that lane's sample entry, and a failing lane encoder is
    /// an error, not ffmpeg. Whether the lanes' entries are equal (one `hvc1`) depends on the
    /// driver: radeonsi rewrites the SPS / PPS, iHD writes ours (S8 picks `hvc1` or `hev1`).
    #[test]
    fn lanes_on_every_gpu_join_into_one_stream() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (w, h, n) = (320u32, 192u32, 6u64);
        let s = ExportSettings {
            format: Format::Hevc,
            hevc_bit_depth: 10,
            bitrate_mode: filmcraft_export::BitrateMode::Crf,
            crf: 24.0,
            keyframe_distance: Some(30),
            ..Default::default()
        };
        let rate = FrameRate::new(24, 1);
        let mut nodes: Vec<Option<String>> = vec![None];
        nodes.extend(
            crate::va::render_nodes()
                .into_iter()
                .filter(|n| {
                    crate::va::Display::for_node(n)
                        .is_ok_and(|d| d.supports(crate::va_sys::VAProfile_VAProfileHEVCMain10, crate::va_sys::VAEntrypoint_VAEntrypointEncSlice))
                })
                .map(Some),
        );
        if HevcEncoder::lane_stream(w, h, rate, &s, &[None]).is_none() {
            eprintln!("no in-process encoder; skipped");
            return;
        }
        // `None`: the drivers can't write one stream (radeonsi + iHD, fork 6E.3 S8): each lane
        // keeps its driver's own parameter sets and the file carries them in band (hev1)
        let stream = HevcEncoder::lane_stream(w, h, rate, &s, &nodes);
        eprintln!("lanes {nodes:?}: one shared stream: {}", stream.is_some());
        assert!(HevcEncoder::lane_stream(w, h, rate, &s, &[Some("/dev/dri/no-such-node".into())]).is_none());
        let mut entries = Vec::new();
        let mut firsts = Vec::new();
        let mut joined = Vec::new();
        for node in &nodes {
            let lane = LaneTarget { node: node.clone(), stream: stream.clone(), in_band: stream.is_none() };
            let mut enc = HevcEncoder::for_lane(w, h, rate, &s, lane).expect("lane encoder");
            let entry = enc.sample_entry();
            let mut chunk = Vec::new();
            for i in 0..n {
                let rgba: Vec<u8> = (0..w * h).flat_map(|_| [(i * 30) as u8, 90, 160, 255]).collect();
                chunk.extend(enc.encode(&EncoderFrame { width: w, height: h, rgba: &rgba, hdr: None, yuv: None, gpu: None, index: i }).expect("encode"));
            }
            chunk.extend(enc.flush().expect("flush"));
            assert_eq!(chunk.len() as u64, n, "{node:?}");
            assert!(chunk.first().is_some_and(|p| p.key), "a chunk starts with an IDR");
            firsts.push(chunk.first().map(|p| p.data.clone()).unwrap_or_default());
            joined.extend(chunk.iter().map(|p| p.data.clone()));
            let mut d = filmcraft_codecs::software_video_decoder(&entry).expect("decoder");
            let mut frames = Vec::new();
            for (k, p) in chunk.iter().enumerate() {
                frames.extend(d.decode(&p.data, k as i64 * 1000 + p.composition_offset as i64).expect("decode"));
            }
            frames.extend(d.flush());
            assert_eq!(frames.len() as u64, n, "{node:?}");
            entries.push(entry);
        }
        if stream.is_some() {
            assert!(entries.windows(2).all(|w| w[0] == w[1]), "lanes {nodes:?}: one hvcC for all");
        }
        // in band: hev1, and each chunk's IDR carries its own VPS / SPS / PPS
        let nal_types = |au: &[u8]| {
            let mut out = Vec::new();
            let mut rest = au;
            while let (Some(len), Some(tail)) = (rest.get(..4), rest.get(4..)) {
                let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
                out.push(tail.first().map_or(0, |b| (b >> 1) & 0x3f));
                rest = tail.get(len..).unwrap_or(&[]);
            }
            out
        };
        for (e, first) in entries.iter().zip(&firsts) {
            assert_eq!(&e.format.0, if stream.is_some() { b"hvc1" } else { b"hev1" });
            assert_eq!(nal_types(first).iter().filter(|t| (32..=34).contains(*t)).count(), if stream.is_some() { 0 } else { 3 });
        }
        // the joined file: one sample entry (the first lane's), every chunk decodes after it
        let mut d = filmcraft_codecs::software_video_decoder(&entries[0]).expect("decoder");
        let mut frames = 0;
        for (k, p) in joined.iter().enumerate() {
            frames += d.decode(p, k as i64 * 1000).expect("decode joined").len();
        }
        frames += d.flush().len();
        assert_eq!(frames, joined.len(), "every picture of every chunk");
        // a lane's encoder failing mid-chunk is an error for the coordinator
        FAIL_AFTER.store(3, std::sync::atomic::Ordering::SeqCst);
        let lane = LaneTarget { node: None, in_band: stream.is_none(), stream };
        let mut enc = HevcEncoder::for_lane(w, h, rate, &s, lane).expect("lane encoder");
        let rgba = vec![128u8; (w * h * 4) as usize];
        let mut failed = false;
        for i in 0..12 {
            if enc.encode(&EncoderFrame { width: w, height: h, rgba: &rgba, hdr: None, yuv: None, gpu: None, index: i }).is_err() {
                failed = true;
                break;
            }
        }
        failed = failed || enc.flush().is_err();
        FAIL_AFTER.store(0, std::sync::atomic::Ordering::SeqCst);
        assert!(failed, "the failure reaches the caller");
        assert_eq!(enc.backend(), Backend::VaNative, "no ffmpeg fallback in a lane");
        // 4:2:2 has no two-GPU path
        let s422 = ExportSettings { hevc_chroma_422: true, ..s };
        assert!(HevcEncoder::for_lane(w, h, rate, &s422, LaneTarget::default()).is_err());
    }

    /// The in-process encoder failing mid-stream (simulated at frame 11, with pictures still in
    /// flight): ffmpeg encodes the unfinished frames again and carries on; every frame comes out
    /// once, in order. Skipped without the in-process encoder or ffmpeg's encoders.
    #[test]
    fn a_failing_in_process_encoder_falls_back_to_ffmpeg() {
        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (w, h, n) = (320u32, 192u32, 30u64);
        let s = ExportSettings { format: Format::Hevc, hevc_bit_depth: 8, bitrate_kbps: 4000, keyframe_distance: Some(12), ..Default::default() };
        FAIL_AFTER.store(11, std::sync::atomic::Ordering::SeqCst);
        let Ok(mut enc) = HevcEncoder::with(w, h, FrameRate::new(24, 1), &s, &[Backend::VaNative]) else {
            eprintln!("no in-process encoder; skipped");
            return;
        };
        let (has_vaapi, has_x265) = encoders();
        if !has_vaapi && !has_x265 {
            eprintln!("no ffmpeg HEVC encoder; skipped");
            return;
        }
        let level = |i: u64| (16 + (i * 9) % 220) as u8;
        let mut packets = Vec::new();
        for i in 0..n {
            let rgba: Vec<u8> = (0..w * h).flat_map(|_| [level(i), level(i), level(i), 255]).collect();
            packets.extend(enc.encode(&EncoderFrame { width: w, height: h, rgba: &rgba, hdr: None, yuv: None, gpu: None, index: i }).expect("encode"));
        }
        packets.extend(enc.flush().expect("flush"));
        assert_ne!(enc.backend(), Backend::VaNative, "fell back");
        assert_eq!(packets.len() as u64, n);
        let mut d = filmcraft_codecs::software_video_decoder(&enc.sample_entry()).expect("decoder");
        let mut frames = Vec::new();
        for (k, p) in packets.iter().enumerate() {
            frames.extend(d.decode(&p.data, k as i64 * 1000 + p.composition_offset as i64).expect("decode"));
        }
        frames.extend(d.flush());
        assert_eq!(frames.len() as u64, n);
        for (i, f) in frames.iter().enumerate() {
            let PixelData::Yuv8 { planes, .. } = &f.frame.data else { panic!("8-bit planar") };
            let mean = planes[0].iter().map(|&v| v as f64).sum::<f64>() / planes[0].len() as f64;
            let want = 16.0 + 219.0 * level(i as u64) as f64 / 255.0;
            assert!((mean - want).abs() < 3.0, "frame {i}: luma {mean:.1}, want {want:.1}");
        }
    }

    /// Fork 6E.3 S10 (DESIGN-6E3 §2.7): the second GPU's real encoder fails mid-chunk; its chunk
    /// goes to the first GPU and the export still has every frame once, decodable, with the reason
    /// in the lanes report. A cancelled two-GPU export leaves no spill directory behind. Skips
    /// without two VA-API HEVC encoders.
    #[test]
    fn a_failing_lane_hands_its_chunks_to_the_other_gpu() {
        use filmcraft_media::generators::GeneratorSource;
        use filmcraft_media::{Generator, MediaSource, SharedSource};
        use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
        use filmcraft_render::{SourceMap, SourceProvider};
        use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};
        use std::sync::atomic::Ordering;

        struct Lanes(Arc<SourceMap>);
        impl SourceProvider for Lanes {
            fn source(&self, item: ItemId) -> Option<SharedSource> {
                self.0.source(item)
            }
            fn lane_copy(&self) -> Option<Arc<dyn SourceProvider + Send + Sync>> {
                Some(Arc::new(Lanes(self.0.clone())))
            }
        }

        let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = crate::register();
        crate::export_lanes::register();
        let Some(second) = crate::export_lanes::second_encode_node() else {
            eprintln!("SKIPPED: no second GPU with an HEVC encoder");
            return;
        };
        // 6 s of bars, 24 fps: six one-second GOPs
        let mut p = Project::new("fail");
        let r = FrameRate::FPS_24;
        let len = Tick(6 * TICKS_PER_SECOND);
        let g = GeneratorSource::new(Generator::BarsAndTone, 320, 180, r, len);
        let info = g.info().clone();
        let clip = MediaClip {
            media: MediaRef::Generator(g.generator.clone()),
            info: info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        let id = p.add_item(&info.name, Label::Iris, ItemKind::Media(clip), None);
        let mut m = SourceMap::default();
        m.0.insert(id, Arc::new(g));
        let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: r, ..Default::default() }, 1, 0, None);
        let v = p.make_track_item(id, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, len), r).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        let src = Lanes(Arc::new(m));
        let dir = std::env::temp_dir().join(format!("fc-dual-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fail.mp4").to_string_lossy().into_owned();
        let s = ExportSettings {
            format: Format::Hevc,
            path: out.clone(),
            hevc_bit_depth: 8,
            keyframe_distance: Some(24),
            export_engines: filmcraft_export::ExportEngines::Dual,
            ..Default::default()
        };
        let spill_dirs = || std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".chunks")).count();

        *FAIL_NODE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(second.clone());
        FAIL_AFTER.store(5, Ordering::SeqCst);
        let progress = filmcraft_export::Progress::default();
        let p = Arc::new(p);
        let result = filmcraft_export::export(&p, seq, &s, &src, &progress);
        FAIL_AFTER.store(0, Ordering::SeqCst);
        *FAIL_NODE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let report = filmcraft_export::lanes::last_lanes();
        eprintln!("{report:?}");
        let result = result.expect("the export survives a failing lane");
        if report.lanes.is_empty() {
            eprintln!("SKIPPED: {}", report.note.unwrap_or_default());
            return;
        }
        assert_eq!(result.frames, 144);
        assert_eq!(progress.done.load(Ordering::Relaxed), 144);
        let failed = report.lanes.iter().find(|l| l.node.as_deref() == Some(second.as_str())).expect("the second GPU's lane");
        assert!(failed.failed.as_deref().is_some_and(|why| !why.is_empty()), "the reason is reported: {report:?}");
        assert_eq!(report.lanes.iter().map(|l| l.frames).sum::<u64>(), 144, "every frame once: {report:?}");
        assert_eq!(spill_dirs(), 0, "the spill directory is removed");
        let bytes = std::fs::read(&out).unwrap();
        let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
        let vt = file.tracks.iter().position(|t| t.kind == filmcraft_isobmff::TrackKind::Video).unwrap();
        let mut d = filmcraft_codecs::software_video_decoder(&file.tracks[vt].entries[0]).unwrap();
        let mut decoded = 0;
        for (k, smp) in file.tracks[vt].samples.iter().enumerate() {
            decoded += d.decode(&file.read_sample(&bytes, vt, k).unwrap(), smp.pts).unwrap().len();
        }
        decoded += d.flush().len();
        assert_eq!(decoded, 144, "every frame decodes");

        // cancelled part-way: an error, and no chunk directory left behind
        let progress = Arc::new(filmcraft_export::Progress::default());
        let watch = progress.clone();
        let canceller = std::thread::spawn(move || {
            let t = std::time::Instant::now();
            while watch.done.load(Ordering::Relaxed) < 30 && t.elapsed() < std::time::Duration::from_secs(60) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            watch.cancel.store(true, Ordering::SeqCst);
        });
        let cancelled = filmcraft_export::export(&p, seq, &s, &src, &progress);
        let _ = canceller.join();
        assert!(cancelled.is_err() || progress.done.load(Ordering::Relaxed) == 144, "cancel stops the export");
        assert_eq!(spill_dirs(), 0, "cancel removes the spill directory");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
