//! Fork 8.6d: an NVENC H.265 export whose encoder fails part-way (a lost GPU, a driver reset)
//! carries on in the in-process VA-API encoder instead of ending with an error.
//!
//! The two encoders write different streams, so the switch happens at an IDR picture: NVENC's
//! packets are written only once every earlier picture has come out (with B-frames a P picture
//! comes out before the B pictures shown before it), and the planar copy of every picture not
//! written yet is kept. When NVENC fails, the packets it gave since the last such point are
//! dropped and those pictures go again, in order, into a VA-API encoder whose stream starts with
//! an IDR picture and carries its parameter sets in band. VA-API writes no B-frames, so its
//! packets are shifted by NVENC's reordering delay: the decoding times stay one per frame and the
//! presentation times stay where the file's edit list put them.

use std::collections::{BTreeMap, BTreeSet};

use filmcraft_export::{EncodedPacket, EncoderFrame, ExportError, Result, VideoEncoder};

/// A picture kept for a replay: its planar bytes, or a frame NVENC encoded from GPU memory
/// (fork 8.6d D2: kept as it is, read back only if VA-API has to take over).
pub enum Kept {
    Planar(Vec<u8>),
    Gpu(std::sync::Arc<dyn filmcraft_export::GpuPicture>),
}

/// Makes the encoder that carries on (VA-API in process, parameter sets in band).
pub type MakeContinuation = Box<dyn FnMut() -> Result<Box<dyn VideoEncoder>> + Send>;

/// Pictures kept for a replay, at most (NVENC holds 8 in flight, plus its B-frame delay): more
/// means NVENC's output order is not what it promised.
const MAX_KEPT: usize = 32;

/// The NVENC → VA-API switch of one export.
pub(super) struct Fallback {
    make: MakeContinuation,
    width: u32,
    height: u32,
    /// Added to the continuation's composition offsets: NVENC's reordering delay in timescale units.
    shift: i32,
    /// NVENC's packets in decoding order, with their picture index, until every earlier picture is out.
    held: Vec<(u64, EncodedPacket)>,
    /// Pictures out of NVENC at or past `next`.
    out: BTreeSet<u64>,
    /// Planar copies of the pictures at or past `next`.
    kept: BTreeMap<u64, Kept>,
    /// Every picture before this one is written (`None` until the first picture).
    next: Option<u64>,
    /// The encoder that carries on, once NVENC failed.
    cont: Option<Box<dyn VideoEncoder>>,
}

impl Fallback {
    pub fn new(make: MakeContinuation, width: u32, height: u32, shift: i32) -> Self {
        Fallback { make, width, height, shift, held: Vec::new(), out: BTreeSet::new(), kept: BTreeMap::new(), next: None, cont: None }
    }

    /// Whether VA-API has taken over.
    pub fn has_taken_over(&self) -> bool {
        self.cont.is_some()
    }

    /// The encoder that carries on, once NVENC failed.
    pub fn continuation(&mut self) -> Option<&mut Box<dyn VideoEncoder>> {
        self.cont.as_mut()
    }

    /// Picture `index` goes into NVENC now.
    pub fn submitted(&mut self, index: u64, picture: Kept) -> std::result::Result<(), String> {
        if self.kept.len() >= MAX_KEPT {
            return Err(format!("NVENC holds more than {MAX_KEPT} pictures back"));
        }
        let next = *self.next.get_or_insert(index);
        if index < next || self.kept.contains_key(&index) {
            return Err(format!("picture {index} again"));
        }
        self.kept.insert(index, picture);
        Ok(())
    }

    /// NVENC's packets that came out (decoding order, each with its picture index): the ones that
    /// can be written now.
    pub fn came_out(&mut self, packets: Vec<(u64, EncodedPacket)>) -> Vec<EncodedPacket> {
        for (index, p) in packets {
            self.out.insert(index);
            self.held.push((index, p));
        }
        let Some(mut next) = self.next else { return Vec::new() };
        while self.out.remove(&next) {
            next = next.saturating_add(1);
        }
        self.next = Some(next);
        self.kept.retain(|k, _| *k >= next);
        // a decoding-order prefix showing only pictures before `next` refers to nothing later
        let n = self.held.iter().take_while(|(index, _)| *index < next).count();
        self.held.drain(..n).map(|(_, p)| p).collect()
    }

    /// NVENC failed (`why`): the pictures not written yet go into a new VA-API encoder, which takes
    /// every picture from now on. Its packets so far.
    pub fn take_over(&mut self, why: &str) -> Result<Vec<EncodedPacket>> {
        let mut cont = (self.make)().map_err(|e| ExportError::Encode(format!("NVENC: {why}; VA-API could not carry on: {e}")))?;
        self.held.clear();
        self.out.clear();
        let kept = std::mem::take(&mut self.kept);
        log::warn!("HEVC export: NVENC: {why}; continuing with VA-API (in process) ({} frames again)", kept.len());
        let mut packets = Vec::new();
        for (index, picture) in &kept {
            let (yuv, gpu) = match picture {
                Kept::Planar(p) => (Some(p.as_slice()), None),
                Kept::Gpu(g) => (None, Some(std::sync::Arc::clone(g))),
            };
            let f = EncoderFrame { width: self.width, height: self.height, rgba: &[], hdr: None, yuv, gpu, index: *index };
            packets.extend(cont.encode(&f)?);
        }
        self.cont = Some(cont);
        Ok(self.shifted(packets))
    }

    /// The continuation's packets on NVENC's timeline.
    pub fn shifted(&self, mut packets: Vec<EncodedPacket>) -> Vec<EncodedPacket> {
        for p in &mut packets {
            p.composition_offset = p.composition_offset.saturating_add(self.shift);
        }
        packets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_isobmff::SampleEntry;
    use std::sync::{Arc, Mutex};

    fn packet(tag: u8, key: bool) -> EncodedPacket {
        EncodedPacket { data: vec![tag], key, duration: 1, composition_offset: 0 }
    }

    /// Records the pictures it was given; one packet each, tagged 100 + index.
    struct Recorder(Arc<Mutex<Vec<(u64, Vec<u8>)>>>);

    impl VideoEncoder for Recorder {
        fn sample_entry(&self) -> SampleEntry {
            SampleEntry::hevc(Default::default(), 2, 2)
        }
        fn timescale(&self) -> u32 {
            24
        }
        fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
            self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((f.index, f.yuv.unwrap_or_default().to_vec()));
            Ok(vec![packet(100 + f.index as u8, f.index == 0)])
        }
        fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
            Ok(Vec::new())
        }
    }

    fn fallback() -> (Fallback, Arc<Mutex<Vec<(u64, Vec<u8>)>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        (Fallback::new(Box::new(move || Ok(Box::new(Recorder(s.clone())) as Box<dyn VideoEncoder>)), 2, 2, 7), seen)
    }

    /// With one B-frame NVENC gives I0, P2, B1, P4, B3…: P2 waits for B1, so a failure after P2
    /// came out replays pictures 1 and 2, never writing P2 without B1.
    #[test]
    fn packets_wait_until_every_earlier_picture_is_out() {
        let (mut fb, seen) = fallback();
        for i in 0..5u64 {
            fb.submitted(i, Kept::Planar(vec![i as u8])).expect("kept");
        }
        assert_eq!(fb.came_out(vec![(0, packet(0, true))]).len(), 1, "I0 at once");
        assert!(fb.came_out(vec![(2, packet(2, false))]).is_empty(), "P2 waits for B1");
        let replay = fb.take_over("lost").expect("carries on");
        let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        assert_eq!(seen.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1, 2, 3, 4], "every picture not written, in order");
        assert_eq!(seen[0].1, vec![1u8], "its own bytes");
        assert_eq!(replay.len(), 4);
        assert!(replay.iter().all(|p| p.composition_offset == 7), "on NVENC's timeline");
        assert!(fb.continuation().is_some());
    }

    #[test]
    fn a_complete_prefix_is_written_in_decoding_order() {
        let (mut fb, _) = fallback();
        for i in 0..5u64 {
            fb.submitted(i, Kept::Planar(vec![])).expect("kept");
        }
        let got = fb.came_out(vec![(0, packet(0, true)), (2, packet(2, false)), (1, packet(1, false))]);
        assert_eq!(got.iter().map(|p| p.data[0]).collect::<Vec<_>>(), vec![0, 2, 1]);
        assert_eq!(fb.kept.keys().copied().collect::<Vec<_>>(), vec![3, 4], "written pictures are let go");
        let got = fb.came_out(vec![(4, packet(4, false)), (3, packet(3, false))]);
        assert_eq!(got.len(), 2);
        assert!(fb.kept.is_empty());
    }

    #[test]
    fn hostile_orders_are_refused_not_kept_forever() {
        let (mut fb, _) = fallback();
        fb.submitted(5, Kept::Planar(vec![])).expect("first");
        assert!(fb.submitted(5, Kept::Planar(vec![])).is_err(), "the same picture twice");
        assert!(fb.submitted(4, Kept::Planar(vec![])).is_err(), "before the first");
        for i in 6..(5 + MAX_KEPT as u64) {
            fb.submitted(i, Kept::Planar(vec![])).expect("room");
        }
        assert!(fb.submitted(99, Kept::Planar(vec![])).is_err(), "bounded");
    }

    #[test]
    fn no_continuation_is_an_error_naming_both() {
        let mut fb = Fallback::new(Box::new(|| Err(ExportError::Unsupported("no VA-API encoder".into()))), 2, 2, 0);
        fb.submitted(0, Kept::Planar(vec![])).expect("kept");
        let e = fb.take_over("the GPU was lost").err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("NVENC") && e.contains("VA-API"), "{e}");
    }
}
