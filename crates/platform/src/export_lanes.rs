//! The GPUs of a two-GPU HEVC export (fork 6E.3): this machine's side of
//! [`filmcraft_export::lanes::LaneDevices`]. The primary lane is the process's VA-API displays
//! (AMD here); the second is another render node whose driver encodes HEVC (Intel here).
//!
//! Fork 8.6d: where exports render on an NVIDIA GPU (whose VA-API is not used) and the process's
//! VA-API encode device is another GPU (the XPS 16 on Performance: RTX 4050 + Arc), the pair is
//! NVENC on the NVIDIA GPU (the primary lane, rendering where the export renders) and VA-API on
//! that device, which renders its own chunks. NVENC's and VA-API's parameter sets differ, so such
//! an export is `hev1`.

use std::sync::Arc;

use filmcraft_export::lanes::{LaneDevices, LaneEncoders};
use filmcraft_export::{ExportError, ExportSettings, GpuFrames, Result, VideoEncoder};
use filmcraft_time::FrameRate;

use filmcraft_export::HevcEncoder as Choice;

use crate::hevc_export::{HevcEncoder, LaneTarget};
use crate::va::Display;
use crate::va_sys as sys;

/// Register the two-GPU export devices (once; harmless where there is one GPU: `second_node`
/// then finds none and every export stays single).
pub fn register() {
    filmcraft_export::lanes::register_lane_devices(Box::new(VaLanes));
}

/// A render node other than the process's encode device whose VA-API driver encodes HEVC (Main
/// or Main 10): `FILMCRAFT_EXPORT_SECOND_DEVICE` when set (tests), else the first such node.
pub fn second_encode_node() -> Option<String> {
    let primary = Display::for_encode().ok()?.node.clone();
    let candidates = match std::env::var("FILMCRAFT_EXPORT_SECOND_DEVICE").ok().filter(|v| !v.is_empty()) {
        Some(n) => vec![n],
        None => crate::va::render_nodes(),
    };
    candidates.into_iter().filter(|n| *n != primary).find(|n| encodes_hevc(n))
}

const VENDOR_NVIDIA: u32 = 0x10de;

/// Fork 8.6d: the render node of the VA-API lane next to an NVENC lane, when exports render on an
/// NVIDIA GPU that encodes H.265 and the process's VA-API encode device is another GPU's. Asks
/// NVENC only when exports already render on NVIDIA (so a sleeping NVIDIA GPU is never woken).
pub fn nvenc_pair_node() -> Option<String> {
    nvenc_pair(filmcraft_gpu::adapter_pick::chosen_vendor(), || Display::for_encode().ok().map(|d| d.node.clone()), crate::nvenc::hevc_available)
}

fn nvenc_pair(render_vendor: Option<u32>, encode_node: impl FnOnce() -> Option<String>, nvenc: impl FnOnce() -> bool) -> Option<String> {
    if render_vendor != Some(VENDOR_NVIDIA) {
        return None;
    }
    let node = encode_node()?;
    let other_gpu = crate::gpu_export::pci_ids(&node).is_some_and(|(v, _)| v != VENDOR_NVIDIA);
    (other_gpu && encodes_hevc(&node) && nvenc()).then_some(node)
}

fn encodes_hevc(node: &str) -> bool {
    Display::for_node(node).is_ok_and(|d| {
        d.supports(sys::VAProfile_VAProfileHEVCMain, sys::VAEntrypoint_VAEntrypointEncSlice)
            || d.supports(sys::VAProfile_VAProfileHEVCMain10, sys::VAEntrypoint_VAEntrypointEncSlice)
    })
}

fn display(node: Option<&str>) -> std::result::Result<Arc<Display>, String> {
    match node {
        Some(n) => Display::for_node(n),
        None => Display::for_encode(),
    }
}

struct VaLanes;

impl LaneDevices for VaLanes {
    fn second_node(&self) -> Option<String> {
        // diagnostic (fork U0.2.3b d): the second lane on a chosen render node, e.g. the primary
        // GPU's own (two decode / encode sessions on one GPU)
        if let Some(n) = std::env::var("FILMCRAFT_EXPORT_SECOND_NODE").ok().filter(|n| n.starts_with("/dev/dri/renderD")) {
            return Some(n);
        }
        second_encode_node().or_else(nvenc_pair_node)
    }

    fn name(&self, node: Option<&str>) -> String {
        if node.is_none() && nvenc_pair_node().is_some() {
            return "NVIDIA".into();
        }
        let node = match node {
            Some(n) => n.to_string(),
            None => match Display::for_encode() {
                Ok(d) => d.node.clone(),
                Err(_) => return "GPU".into(),
            },
        };
        match crate::gpu_export::pci_ids(&node).map(|(v, _)| v) {
            Some(0x1002) => "AMD".into(),
            Some(0x8086) => "Intel".into(),
            Some(0x10de) => "NVIDIA".into(),
            _ => "GPU".into(),
        }
    }

    fn frames_on(&self, node: &str) -> Option<Box<dyn GpuFrames>> {
        crate::gpu_export::frames_on(node)
    }

    fn bind_thread(&self, node: Option<&str>) {
        filmcraft_codecs::hw::set_thread_decode_node(node.map(str::to_string));
    }

    fn bind_thread_handoff(&self, node: &str) {
        filmcraft_codecs::hw::set_thread_decode_handoff(Some(node.to_string()));
    }

    fn decodes_422(&self, node: Option<&str>) -> bool {
        let d = match node {
            Some(n) => Display::for_node(n),
            None => Display::get(),
        };
        d.is_ok_and(|d| d.supports(sys::VAProfile_VAProfileHEVCMain422_10, sys::VAEntrypoint_VAEntrypointVLD))
    }

    fn decodes_for_primary(&self, node: &str) -> bool {
        // the NVENC pair's primary renders on NVIDIA, which can't take Intel's tiled 4:2:0 pictures
        nvenc_pair_node().as_deref() != Some(node)
    }

    fn encoders(&self, w: u32, h: u32, rate: FrameRate, settings: &ExportSettings, nodes: &[Option<String>]) -> Option<Arc<dyn LaneEncoders>> {
        if let [None, Some(second)] = nodes
            && nvenc_pair_node().as_ref() == Some(second)
        {
            return NvencVaLanes::new(w, h, rate, settings, second).map(|l| Arc::new(l) as Arc<dyn LaneEncoders>);
        }
        if settings.hevc_chroma_422 || nodes.iter().any(|n| display(n.as_deref()).is_err()) {
            return None;
        }
        let stream = HevcEncoder::lane_stream(w, h, rate, settings, nodes);
        Some(Arc::new(VaLaneEncoders { w, h, rate, settings: settings.clone(), nodes: nodes.to_vec(), stream }))
    }
}

struct VaLaneEncoders {
    w: u32,
    h: u32,
    rate: FrameRate,
    settings: ExportSettings,
    nodes: Vec<Option<String>>,
    /// The stream every lane can write (`None`: the drivers share none, so in band).
    stream: Option<filmcraft_hevc::write::StreamParams>,
}

impl LaneEncoders for VaLaneEncoders {
    fn shared_stream(&self) -> bool {
        self.stream.is_some()
    }

    fn make(&self, lane: usize, in_band: bool) -> Result<Box<dyn VideoEncoder>> {
        let node = self.nodes.get(lane).ok_or_else(|| ExportError::Encode(format!("no lane {lane}")))?.clone();
        // in band, each driver writes its own stream (its own parameter sets in every IDR)
        let in_band = in_band || self.stream.is_none();
        let target = LaneTarget { node, stream: if in_band { None } else { self.stream.clone() }, in_band };
        HevcEncoder::for_lane(self.w, self.h, self.rate, &self.settings, target).map(|e| Box::new(e) as Box<dyn VideoEncoder>)
    }
}

/// Fork 8.6d: lane 0 NVENC (the GPU exports render on), lane 1 VA-API on `node`; parameter sets in
/// band (`hev1`), no B-frames in either (the same timing in every chunk).
struct NvencVaLanes {
    w: u32,
    h: u32,
    rate: FrameRate,
    settings: ExportSettings,
    node: String,
}

impl NvencVaLanes {
    fn new(w: u32, h: u32, rate: FrameRate, settings: &ExportSettings, node: &str) -> Option<Self> {
        // Hardware means VA-API, Software libx265: NVENC only under Auto, as for one GPU
        if settings.hevc_encoder != Choice::Auto || settings.hevc_chroma_422 {
            return None;
        }
        if let Err(why) = crate::nvenc::export::linux_hevc_lane_encoder(w, h, rate, settings) {
            log::info!("two-GPU export: NVENC can't take a lane: {why}");
            return None;
        }
        Some(NvencVaLanes { w, h, rate, settings: settings.clone(), node: node.to_string() })
    }
}

impl LaneEncoders for NvencVaLanes {
    fn shared_stream(&self) -> bool {
        false
    }

    fn make(&self, lane: usize, _in_band: bool) -> Result<Box<dyn VideoEncoder>> {
        match lane {
            0 => {
                crate::nvenc::export::linux_hevc_lane_encoder(self.w, self.h, self.rate, &self.settings).map_err(|e| ExportError::Encode(format!("NVENC: {e}")))
            }
            1 => {
                let target = LaneTarget { node: Some(self.node.clone()), stream: None, in_band: true };
                HevcEncoder::for_lane(self.w, self.h, self.rate, &self.settings, target).map(|e| Box::new(e) as Box<dyn VideoEncoder>)
            }
            _ => Err(ExportError::Encode(format!("no lane {lane}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fork 8.6d: the NVENC + VA-API pair only where exports render on NVIDIA and the VA-API encode
    /// device is another GPU's; NVENC is never asked otherwise (it would wake a sleeping RTX 4050).
    #[test]
    fn nvenc_pairs_only_when_exports_render_on_nvidia() {
        let never = || -> bool { panic!("NVENC must not be asked") };
        let no_node = || -> Option<String> { panic!("no VA-API device needed") };
        assert_eq!(nvenc_pair(Some(0x8086), no_node, never), None, "Power saving: the Arc renders");
        assert_eq!(nvenc_pair(None, no_node, never), None, "no GPU known");
        assert_eq!(nvenc_pair(Some(0x1002), no_node, never), None, "AMD renders");
        assert_eq!(nvenc_pair(Some(VENDOR_NVIDIA), || None, never), None, "no VA-API encode device");
        // a node that isn't a render node (no PCI ids): never paired, NVENC not asked
        assert_eq!(nvenc_pair(Some(VENDOR_NVIDIA), || Some("/dev/null".into()), never), None);
    }
}
