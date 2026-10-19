//! Exporting one range on several GPUs at once (fork 6E.3, `docs/fork/DESIGN-6E3.md`): the range
//! is cut into chunks, each GPU's lane encodes whole chunks, and the chunks are joined in order.
//!
//! The export crate knows no GPU: the platform layer registers [`LaneDevices`] (the second
//! render node, a renderer and encoders on it, the per-thread decode binding). Without it every
//! export runs on one GPU as before.

pub mod chunker;
pub mod coordinator;
pub(crate) mod lane;
pub mod scheduler;
pub mod spill;

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use filmcraft_isobmff::SampleEntry;
use filmcraft_time::FrameRate;
use serde::Serialize;

use crate::{ExportSettings, GpuFrames, Result, VideoEncoder};

/// The GPUs a two-GPU export can use (fork 6E.3), registered by the platform layer. `node`
/// `None` is the primary lane: the process's own decode / encode displays.
pub trait LaneDevices: Send + Sync {
    /// The render node of a second GPU, other than the process's encode device, whose encoder can
    /// take HEVC; `None`: one GPU.
    fn second_node(&self) -> Option<String>;
    /// A short name of the GPU behind `node` ("AMD", "Intel") for the report.
    fn name(&self, node: Option<&str>) -> String;
    /// An export renderer of its own on `node`'s GPU (`None`: its frames render on the CPU).
    fn frames_on(&self, node: &str) -> Option<Box<dyn GpuFrames>>;
    /// Hardware decoders made on the calling thread from now on decode on `node`'s GPU.
    fn bind_thread(&self, node: Option<&str>);
    /// Hardware decoders made on the calling thread from now on decode on `node`'s GPU and hand
    /// their pictures to the primary GPU (fork U0.2.3b d: Intel decodes, AMD renders and
    /// encodes). The default decodes on the primary GPU.
    fn bind_thread_handoff(&self, node: &str) {
        let _ = node;
        self.bind_thread(None);
    }
    /// Whether `node`'s GPU may decode for an export the primary GPU renders and encodes alone
    /// ([`lane::start`]'s decode-only plan). Fork 8.6d: not where the primary is an NVIDIA GPU next
    /// to `node`'s Intel one (NVIDIA imports none of Intel's tiled 4:2:0 pictures).
    fn decodes_for_primary(&self, node: &str) -> bool {
        let _ = node;
        true
    }
    /// Whether `node`'s GPU decodes HEVC 4:2:2 (the lane 4:2:2-source chunks prefer).
    fn decodes_422(&self, node: Option<&str>) -> bool;
    /// The encoders of lanes on `nodes` for this export; `None` when they can't take it.
    fn encoders(&self, w: u32, h: u32, rate: FrameRate, settings: &ExportSettings, nodes: &[Option<String>]) -> Option<Arc<dyn LaneEncoders>>;
}

/// The encoders of one two-GPU export (one per chunk: each starts with an IDR).
pub trait LaneEncoders: Send + Sync {
    /// Whether the lanes' drivers can write one stream (one `hvc1` sample entry for all chunks).
    fn shared_stream(&self) -> bool;
    /// A fresh encoder for lane `lane`. `in_band`: each driver writes its own parameter sets into
    /// every IDR and the file is `hev1` (lanes that can't share one `hvcC`).
    fn make(&self, lane: usize, in_band: bool) -> Result<Box<dyn VideoEncoder>>;
}

static DEVICES: OnceLock<Box<dyn LaneDevices>> = OnceLock::new();

/// Register the platform's GPUs for two-GPU export (once; later calls are ignored).
pub fn register_lane_devices(d: Box<dyn LaneDevices>) {
    let _ = DEVICES.set(d);
}

/// The registered GPUs (none on platforms without a two-GPU path).
pub fn lane_devices() -> Option<&'static dyn LaneDevices> {
    DEVICES.get().map(|b| b.as_ref())
}

/// Whether one `hvc1` sample entry can describe every lane's chunks: all lanes report the very
/// same entry (codec configuration bytes included). Any difference, or no lanes, means the
/// parameter sets must travel in band (`hev1`), or a decoder would use the wrong ones.
pub fn entries_agree(entries: &[SampleEntry]) -> bool {
    entries.first().is_some_and(|first| entries.iter().all(|e| e == first))
}

/// One lane of the last two-GPU export (`perf.stats` `export.lanes`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneReport {
    pub name: String,
    /// Render node (`None`: the process's own devices).
    pub node: Option<String>,
    /// Chunks it finished.
    pub chunks: u64,
    /// Frames of the chunks it finished.
    pub frames: u64,
    /// Its frames per second of the lane's wall time.
    pub fps: f64,
    /// Why it stopped early because something went wrong (its chunks went to the other lanes).
    pub failed: Option<String>,
    /// Why the slow-lane rule took it off the export while it still worked (its chunk went to
    /// the other lanes). Not an error: `failed` stays `None`.
    pub retired: Option<String>,
}

/// What the last stepped HEVC export did about lanes (`perf.stats` `export.lanes` /
/// `export.sampleEntry`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanesReport {
    /// Empty when the export ran on one GPU.
    pub lanes: Vec<LaneReport>,
    /// `hvc1` / `hev1` of a two-GPU export.
    pub sample_entry: Option<String>,
    /// Why the export ran on one GPU, or what went wrong with the lanes.
    pub note: Option<String>,
}

static LAST: Mutex<Option<LanesReport>> = Mutex::new(None);

/// The last export's lanes (default when none ran yet).
pub fn last_lanes() -> LanesReport {
    LAST.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_default()
}

pub(crate) fn set_last_lanes(r: LanesReport) {
    *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
}

/// The second render node the registered devices offer (`perf.stats` `export.secondEncodeNode`).
pub fn second_encode_node() -> Option<String> {
    lane_devices().and_then(|d| d.second_node())
}

/// `MemAvailable` of `/proc/meminfo` in bytes.
pub(crate) fn mem_available() -> Option<u64> {
    parse_mem_available(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_mem_available(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.trim_start_matches("MemAvailable:").trim().trim_end_matches("kB").trim().parse().ok()?;
    kb.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_isobmff::{HevcConfig, HevcNalArray, PcmConfig, SampleEntry};

    #[test]
    fn mem_available_parses_and_refuses_junk() {
        let m = "MemTotal:       32000000 kB\nMemFree:  100 kB\nMemAvailable:   20000000 kB\n";
        assert_eq!(parse_mem_available(m), Some(20_000_000 * 1024));
        assert_eq!(parse_mem_available("MemAvailable: lots kB"), None);
        assert_eq!(parse_mem_available("MemAvailable: 99999999999999999999 kB"), None);
        assert_eq!(parse_mem_available(""), None);
    }

    fn pcm(bits: u16) -> SampleEntry {
        SampleEntry::pcm(PcmConfig { bits, float: false, big_endian: false, signed: true, channels: 2, sample_rate: 48_000.0 })
    }

    /// FFI review (S7): a one-byte difference between two lanes' entries must send the export to
    /// in-band parameter sets, never write one `hvc1` entry for both.
    #[test]
    fn lanes_share_an_entry_only_when_every_byte_agrees() {
        assert!(entries_agree(&[pcm(16), pcm(16), pcm(16)]));
        assert!(entries_agree(&[pcm(16)]));
        assert!(!entries_agree(&[pcm(16), pcm(24)]));
        assert!(!entries_agree(&[pcm(16), pcm(16), pcm(24)]));
        assert!(!entries_agree(&[]));
        let mut a = pcm(16);
        a.data_reference_index = 2;
        assert!(!entries_agree(&[pcm(16), a]));
        // two `hvc1` entries whose SPS differ in one byte (radeonsi rewriting a field)
        let hvc = |sps_byte: u8| {
            let arrays = vec![
                HevcNalArray { completeness: true, nal_type: 32, nalus: vec![vec![0x40, 1, 0x0c]] },
                HevcNalArray { completeness: true, nal_type: 33, nalus: vec![vec![0x42, 1, 1, sps_byte]] },
                HevcNalArray { completeness: true, nal_type: 34, nalus: vec![vec![0x44, 1, 0xc1]] },
            ];
            SampleEntry::hevc(HevcConfig { general_profile_idc: 2, length_size: 4, arrays, ..Default::default() }, 1920, 1080)
        };
        assert!(entries_agree(&[hvc(0x60), hvc(0x60)]));
        assert!(!entries_agree(&[hvc(0x60), hvc(0x61)]));
    }
}
