//! SAM 2.1's video predictor (`SAM2VideoPredictor` + `SAM2Base.track_step`) for one subject:
//! prompts on any frames become conditioning frames (kept in memory for good), every other
//! frame is tracked from the memory bank (the conditioning frames, the six frames before it in
//! the tracking direction, and up to 16 object pointers), and its result enters the memory in
//! turn.
//!
//! As the reference with the video predictor's post-processing (`apply_postprocessing`):
//! dynamic multimask via stability, binarised masks for prompted frames' memories, holes of
//! ≤ 8 low-res pixels filled. One deliberate difference: a prompt on a frame that was already
//! tracked (a fix stroke) is stored as a conditioning frame (the reference's
//! `add_all_frames_to_correct_as_cond`), so the correction stays in memory for the rest of the
//! clip instead of fading after six frames; the tracked frames within `num_maskmem` of it leave
//! the memory (`clear_non_cond_mem_around_input`), and a fix with a box re-specifies the subject
//! (no memory, no old mask as a prompt) while a fix of points only refines (as the reference).
//! Tracked frames pick among the decoder's candidates with [`Selection`] (default: predicted
//! IoU + overlap with the previous frame, which keeps the whole subject when the decoder briefly
//! prefers a part; [`Selection::Reference`] is the official choice).

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Arc;

use super::config::{D_MODEL, IMAGE_SIZE, MAX_OBJ_PTRS, MEM_DIM};
use super::heads::{HeadOutput, HeadPoint, Sam2Heads};
use super::{EMBED_TOKENS, MEM_MASK_SIDE, Sam2Compute, Sam2Features, bilinear_plane, prepare_input, sine_position};
use crate::video::{Direction, VideoMask, VideoSegmenter};
use crate::{LOW_RES, MaskError, Prompt, RgbImage, Segment};

/// Most conditioning frames the memory attention sees at once (the closest ones, as
/// `select_closest_cond_frames`); more stay stored.
pub const MAX_COND_IN_ATTN: usize = 16;
/// A tracked mask smaller than this share of the nearest prompted frame's is re-anchored.
pub const ANCHOR_SHRINK: f32 = 0.85;
/// A re-anchored answer must hold at least this share of the tracked mask.
pub const ANCHOR_COVER: f32 = 0.9;

/// The inside's area and box (grid pixels, far edges exclusive) of a low-res mask.
fn area_box(m: &[f32]) -> (usize, Option<[usize; 4]>) {
    let (mut n, mut b) = (0usize, [usize::MAX, usize::MAX, 0, 0]);
    for (i, v) in m.iter().enumerate() {
        if *v > 0.0 {
            let (x, y) = (i % LOW_RES, i / LOW_RES);
            n += 1;
            b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
        }
    }
    (n, (n > 0).then_some(b))
}

/// Holes in a mask up to this many low-res pixels are filled (`fill_hole_area`).
const FILL_HOLE_AREA: usize = 8;

/// What the tracker keeps per frame.
#[derive(Clone, Debug)]
struct FrameOut {
    cond: bool,
    /// Memory features, 4096 × 64.
    mem: Vec<f32>,
    obj_ptr: Vec<f32>,
    /// Low-res mask logits (256 × 256), holes filled.
    mask: Vec<f32>,
    iou: f32,
    score: f32,
}

/// How a tracked frame's mask is chosen among the decoder's three candidates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Selection {
    /// As the reference: the best predicted IoU.
    Reference,
    /// Predicted IoU + `weight` × overlap with the previous frame's mask (keeps the whole subject
    /// when the decoder briefly prefers one of its parts).
    Consistent { weight: f32 },
}

impl Default for Selection {
    fn default() -> Self {
        Selection::Consistent { weight: 1.0 }
    }
}

/// SAM 2.1 following one subject through a clip.
pub struct Sam2Tracker {
    selection: Selection,
    /// Re-anchor a tracked frame whose mask collapses (see [`Sam2Tracker::set_anchor`]).
    anchor: bool,
    last_candidates: Vec<(f32, f32)>,
    compute: Arc<dyn Sam2Compute>,
    heads: Arc<Sam2Heads>,
    num_maskmem: usize,
    sigmoid: (f32, f32),
    num_frames: usize,
    frames: BTreeMap<usize, FrameOut>,
    /// Constants: the memory's sine position (4096 × 64), the temporal encodings
    /// (`num_maskmem` × 64), `no_mem_embed` (256), `no_obj_embed_spatial` (64).
    mem_pos: Vec<f32>,
    tpos: Vec<f32>,
    no_mem: Vec<f32>,
    no_obj_spatial: Vec<f32>,
    /// The last frame's features (a prompt right after tracking the same frame reuses them).
    cache: Option<(usize, u64, Arc<Sam2Features>)>,
}

impl std::fmt::Debug for Sam2Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sam2Tracker({} frames in memory)", self.frames.len())
    }
}

/// A cheap fingerprint of a frame (size and a sparse sample), to reuse cached features only for
/// the same picture.
fn fingerprint(image: &RgbImage) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ ((image.width() as u64) << 32 | image.height() as u64);
    for b in image.data().iter().step_by(4099) {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `fill_holes_in_mask_scores`: background regions (logit ≤ 0, 8-connected) of at most `max`
/// pixels inside the mask become 0.1.
fn fill_holes(mask: &mut [f32], side: usize, max: usize) {
    let n = side * side;
    if mask.len() != n {
        return;
    }
    let mut seen = vec![false; n];
    let mut stack = Vec::new();
    let mut region = Vec::new();
    for start in 0..n {
        if seen[start] || mask[start] > 0.0 {
            continue;
        }
        region.clear();
        stack.push(start);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            region.push(i);
            let (x, y) = ((i % side) as i64, (i / side) as i64);
            for dy in -1..=1i64 {
                for dx in -1..=1i64 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= side as i64 || ny >= side as i64 {
                        continue;
                    }
                    let j = (ny as usize) * side + nx as usize;
                    if !seen[j] && mask[j] <= 0.0 {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        if region.len() <= max {
            for i in &region {
                mask[*i] = 0.1;
            }
        }
    }
}

impl Sam2Tracker {
    /// A tracker for a clip of `num_frames` frames (the object pointers' time scale uses it).
    pub fn new(compute: Arc<dyn Sam2Compute>, heads: Arc<Sam2Heads>, num_frames: usize) -> Result<Self, MaskError> {
        let cfg = &heads.weights().cfg;
        let num_maskmem = cfg.num_maskmem;
        let sigmoid = (cfg.sigmoid_scale, cfg.sigmoid_bias);
        let tpos = heads.param("maskmem_tpos_enc")?;
        let no_mem = heads.param("no_mem_embed")?;
        let no_obj_spatial = heads.param("no_obj_embed_spatial")?;
        Ok(Self {
            selection: Selection::default(),
            anchor: true,
            last_candidates: Vec::new(),
            compute,
            heads,
            num_maskmem,
            sigmoid,
            num_frames: num_frames.max(1),
            frames: BTreeMap::new(),
            mem_pos: sine_position(super::EMBED_SIDE, super::EMBED_SIDE, MEM_DIM),
            tpos,
            no_mem,
            no_obj_spatial,
            cache: None,
        })
    }

    /// How tracked frames pick among the decoder's candidates.
    pub fn set_selection(&mut self, s: Selection) {
        self.selection = s;
    }

    /// Re-anchoring (on by default, not in the reference): when a tracked frame's mask shrinks
    /// below [`ANCHOR_SHRINK`] of the nearest prompted frame's, the decoder is asked again on
    /// the frame alone (no memory) with a box prompt (the mask's box widened to the prompted
    /// frame's size, +10 %), and that answer is kept when it holds [`ANCHOR_COVER`] of the
    /// tracked mask and is larger but not much larger than the prompted one. Guards against a
    /// subject partly entering a shadow being cut at the light edge (the memory of the lit
    /// subject no longer matches the shaded part).
    pub fn set_anchor(&mut self, on: bool) {
        self.anchor = on;
    }

    /// The last frame's candidates: (predicted IoU, overlap with the previous mask) (diagnostics).
    pub fn last_candidates(&self) -> &[(f32, f32)] {
        &self.last_candidates
    }

    /// Where the heavy parts run.
    pub fn compute_name(&self) -> String {
        self.compute.name()
    }

    fn features(&mut self, frame: usize, image: &RgbImage) -> Result<Arc<Sam2Features>, MaskError> {
        let fp = fingerprint(image);
        if let Some((f, h, feats)) = &self.cache
            && *f == frame
            && *h == fp
        {
            return Ok(feats.clone());
        }
        let input = prepare_input(image);
        let feats = Arc::new(self.compute.image(&input)?);
        feats.check()?;
        self.cache = Some((frame, fp, feats.clone()));
        Ok(feats)
    }

    /// `_prepare_memory_conditioned_features` for a frame that is not an initial conditioning
    /// frame: the memory bank as tokens (+ position encodings) and how many are pointers.
    fn memory_for(&self, t: usize, reverse: bool) -> Result<(Vec<f32>, Vec<f32>, usize), MaskError> {
        let cond: Vec<(usize, &FrameOut)> = self.frames.iter().filter(|(_, o)| o.cond).map(|(i, o)| (*i, o)).collect();
        if cond.is_empty() {
            return Err(MaskError::BadPrompt("stroke the subject on a frame first".into()));
        }
        // select_closest_cond_frames
        let (selected, unselected): (Vec<(usize, &FrameOut)>, Vec<(usize, &FrameOut)>) = if cond.len() <= MAX_COND_IN_ATTN {
            (cond, Vec::new())
        } else {
            let mut keep: Vec<usize> = Vec::new();
            if let Some((i, _)) = cond.iter().filter(|(i, _)| *i < t).max_by_key(|(i, _)| *i) {
                keep.push(*i);
            }
            if let Some((i, _)) = cond.iter().filter(|(i, _)| *i >= t).min_by_key(|(i, _)| *i) {
                keep.push(*i);
            }
            let mut rest: Vec<usize> = cond.iter().map(|(i, _)| *i).filter(|i| !keep.contains(i)).collect();
            rest.sort_by_key(|i| i.abs_diff(t));
            keep.extend(rest.into_iter().take(MAX_COND_IN_ATTN.saturating_sub(keep.len())));
            cond.into_iter().partition(|(i, _)| keep.contains(i))
        };
        let lookup =
            |i: usize| -> Option<&FrameOut> { self.frames.get(&i).filter(|o| !o.cond).or_else(|| unselected.iter().find(|(j, _)| *j == i).map(|(_, o)| *o)) };
        let step = |base: usize, d: usize| -> Option<usize> { if reverse { base.checked_add(d) } else { base.checked_sub(d) } };
        let mut t_pos_and_prevs: Vec<(usize, &FrameOut)> = selected.iter().map(|(_, o)| (0usize, *o)).collect();
        for t_pos in 1..self.num_maskmem {
            let t_rel = self.num_maskmem - t_pos;
            if let Some(o) = step(t, t_rel).and_then(lookup) {
                t_pos_and_prevs.push((t_pos, o));
            }
        }
        let mut memory = Vec::with_capacity((t_pos_and_prevs.len() * EMBED_TOKENS + 4 * MAX_OBJ_PTRS) * MEM_DIM);
        let mut pos = Vec::with_capacity(memory.capacity());
        for (t_pos, o) in &t_pos_and_prevs {
            if o.mem.len() != EMBED_TOKENS * MEM_DIM {
                return Err(MaskError::Failed("a memory frame has the wrong size".into()));
            }
            memory.extend_from_slice(&o.mem);
            let k = self.num_maskmem - t_pos - 1;
            let tp = self.tpos.get(k * MEM_DIM..(k + 1) * MEM_DIM).ok_or_else(|| MaskError::Failed("temporal encoding missing".into()))?;
            for row in self.mem_pos.as_chunks::<MEM_DIM>().0 {
                pos.extend(row.iter().zip(tp).map(|(a, b)| a + b));
            }
        }
        // object pointers
        let max_ptrs = self.num_frames.min(MAX_OBJ_PTRS);
        let sign: f32 = if reverse { -1.0 } else { 1.0 };
        let mut ptrs: Vec<(f32, &[f32])> = selected
            .iter()
            .filter(|(i, _)| if reverse { *i >= t } else { *i <= t })
            .map(|(i, o)| ((t as f32 - *i as f32) * sign, o.obj_ptr.as_slice()))
            .collect();
        for t_diff in 1..max_ptrs {
            let Some(ti) = step(t, t_diff) else { break };
            if ti >= self.num_frames {
                break;
            }
            if let Some(o) = lookup(ti) {
                ptrs.push((t_diff as f32, o.obj_ptr.as_slice()));
            }
        }
        let t_max = (max_ptrs.saturating_sub(1)).max(1) as f32;
        let split = D_MODEL / MEM_DIM;
        for (p, ptr) in &ptrs {
            let enc = self.heads.ptr_tpos(p / t_max)?;
            if ptr.len() != D_MODEL || enc.len() != MEM_DIM {
                return Err(MaskError::Failed("an object pointer has the wrong size".into()));
            }
            memory.extend_from_slice(ptr);
            for _ in 0..split {
                pos.extend_from_slice(&enc);
            }
        }
        Ok((memory, pos, ptrs.len() * split))
    }

    /// The memory encoder on a frame's result (`_encode_new_memory`).
    fn encode_memory(&self, feats: &Sam2Features, mask: &[f32], score: f32, from_points: bool) -> Result<Vec<f32>, MaskError> {
        let high = bilinear_plane(mask, LOW_RES, LOW_RES, MEM_MASK_SIDE, MEM_MASK_SIDE);
        let (scale, bias) = self.sigmoid;
        let m: Vec<f32> = high
            .iter()
            .map(|v| {
                let p = if from_points { if *v > 0.0 { 1.0 } else { 0.0 } } else { 1.0 / (1.0 + (-v).exp()) };
                p * scale + bias
            })
            .collect();
        let mut mem = self.compute.memory_encoder(&feats.feat, &m)?;
        if mem.len() != EMBED_TOKENS * MEM_DIM || mem.iter().any(|v| !v.is_finite()) {
            return Err(MaskError::Failed("the memory encoder gave a bad result".into()));
        }
        if score <= 0.0 {
            for row in mem.as_chunks_mut::<MEM_DIM>().0 {
                row.iter_mut().zip(&self.no_obj_spatial).for_each(|(a, b)| *a += b);
            }
        }
        Ok(mem)
    }

    fn conditioned(&self, feats: &Sam2Features, t: usize, init: bool, reverse: bool) -> Result<Vec<f32>, MaskError> {
        if init {
            let mut pix = feats.feat.clone();
            for row in pix.as_chunks_mut::<D_MODEL>().0 {
                row.iter_mut().zip(&self.no_mem).for_each(|(a, b)| *a += b);
            }
            return Ok(pix);
        }
        let (memory, pos, ptr_tokens) = self.memory_for(t, reverse)?;
        let pix = self.compute.memory_attention(&feats.feat, &memory, &pos, ptr_tokens)?;
        if pix.len() != EMBED_TOKENS * D_MODEL || pix.iter().any(|v| !v.is_finite()) {
            return Err(MaskError::Failed("memory attention gave a bad result".into()));
        }
        Ok(pix)
    }

    fn output(out: &HeadOutput, mask: &[f32]) -> VideoMask {
        VideoMask { segment: Segment { logits: mask.to_vec(), cover: (1.0, 1.0), score: out.iou.clamp(0.0, 1.0) }, visible: out.object_score > 0.0 }
    }

    /// Drop tracked (not prompted) frames the memory can no longer reach from `t`.
    fn prune(&mut self, t: usize) {
        let reach = MAX_OBJ_PTRS.max(self.num_maskmem) + 1;
        self.frames.retain(|i, o| o.cond || i.abs_diff(t) <= reach);
    }

    /// Prompts in SAM's form: box corners (labels 2, 3) first, then the points, in the 1024
    /// square.
    fn head_points(prompt: &Prompt) -> Result<Vec<HeadPoint>, MaskError> {
        let (points, rect) = prompt.checked()?;
        let s = IMAGE_SIZE as f32;
        let mut v = Vec::with_capacity(points.len() + 2);
        if let Some(r) = rect {
            v.push(HeadPoint { x: r.x0 * s, y: r.y0 * s, label: 2 });
            v.push(HeadPoint { x: r.x1 * s, y: r.y1 * s, label: 3 });
        }
        v.extend(points.iter().map(|p| HeadPoint { x: p.x * s, y: p.y * s, label: i32::from(p.positive) }));
        if v.is_empty() {
            return Err(MaskError::BadPrompt("SAM 2 needs a point or a box".into()));
        }
        Ok(v)
    }

    /// The stored result for a frame, if any (diagnostics).
    pub fn stored(&self, frame: usize) -> Option<(bool, f32)> {
        self.frames.get(&frame).map(|o| (o.cond, o.score))
    }
}

impl VideoSegmenter for Sam2Tracker {
    fn name(&self) -> String {
        format!("SAM 2.1 ({})", self.compute.name())
    }

    fn add_prompt(&mut self, frame: usize, image: &RgbImage, prompt: &Prompt) -> Result<VideoMask, MaskError> {
        let points = Self::head_points(prompt)?;
        let feats = self.features(frame, image)?;
        let tracked = self.frames.contains_key(&frame);
        // A frame never tracked or prompted is an initial conditioning frame (no memory). A fix
        // stroke with a box re-specifies the subject: like an initial frame, without the old
        // mask as a prompt (the reference would feed it back and keep part of the mistake). A
        // fix of points only refines: memory + the frame's previous mask, as the reference.
        // A points-only prompt on an untracked frame of a clip that has prompts already uses the
        // memory too (on its own one click could pick a part of the subject).
        let has_memory = self.frames.values().any(|o| o.cond);
        let init = prompt.rect.is_some() || !has_memory;
        let previous = if init { None } else { self.frames.get(&frame).map(|o| o.mask.iter().map(|v| v.clamp(-32.0, 32.0)).collect::<Vec<f32>>()) };
        let reverse = !init && self.frames.range(..frame).next().is_none() && self.frames.range((Excluded(frame), Unbounded)).next().is_some();
        if tracked {
            // `clear_non_cond_mem_around_input`: the tracked frames around a fix carry the
            // mistake; drop them so the frames after it follow the fix
            let r = self.num_maskmem;
            self.frames.retain(|i, o| o.cond || i.abs_diff(frame) > r);
        }
        let pix = self.conditioned(&feats, frame, init, reverse)?;
        let multimask = points.len() <= 1;
        let out = self.heads.forward(&pix, &feats.s0, &feats.s1, &points, previous.as_deref(), multimask, None)?;
        self.last_candidates = out.candidates.clone();
        let mut mask = out.mask.clone();
        fill_holes(&mut mask, LOW_RES, FILL_HOLE_AREA);
        let mem = self.encode_memory(&feats, &mask, out.object_score, true)?;
        let result = Self::output(&out, &mask);
        self.frames.insert(frame, FrameOut { cond: true, mem, obj_ptr: out.obj_ptr, mask, iou: out.iou, score: out.object_score });
        Ok(result)
    }

    fn propagate(&mut self, frame: usize, image: &RgbImage, direction: Direction) -> Result<VideoMask, MaskError> {
        if let Some(o) = self.frames.get(&frame).filter(|o| o.cond) {
            let seg = Segment { logits: o.mask.clone(), cover: (1.0, 1.0), score: o.iou.clamp(0.0, 1.0) };
            return Ok(VideoMask { segment: seg, visible: o.score > 0.0 });
        }
        if !self.frames.values().any(|o| o.cond) {
            return Err(MaskError::BadPrompt("stroke the subject on a frame first".into()));
        }
        let reverse = direction == Direction::Backward;
        let feats = self.features(frame, image)?;
        let pix = self.conditioned(&feats, frame, false, reverse)?;
        let pad = [HeadPoint { x: 0.0, y: 0.0, label: -1 }];
        // the neighbouring frame in the tracking direction, if it is still in memory
        let neighbour = if reverse { frame.checked_add(1) } else { frame.checked_sub(1) };
        let prev = neighbour.and_then(|n| self.frames.get(&n)).filter(|o| o.score > 0.0).map(|o| o.mask.as_slice());
        let prefer = match self.selection {
            Selection::Reference => None,
            Selection::Consistent { weight } => prev.map(|p| (p, weight)),
        };
        let mut out = self.heads.forward(&pix, &feats.s0, &feats.s1, &pad, None, true, prefer)?;
        self.last_candidates = out.candidates.clone();
        // Re-anchoring: the nearest prompted frame's mask (the user-confirmed size) is the yardstick
        let cond_ref = self.frames.iter().filter(|(_, o)| o.cond && o.score > 0.0).min_by_key(|(i, _)| i.abs_diff(frame)).map(|(_, o)| area_box(&o.mask));
        let (oa, ob) = area_box(&out.mask);
        if self.anchor
            && let Some((ca, Some(cb))) = cond_ref
            && let Some(b) = ob
            && out.object_score > 0.0
            && (oa as f32) < ANCHOR_SHRINK * ca as f32
        {
            // the current box widened to at least the prompted frame's size, grown 10 %
            let (cw, ch) = ((cb[2] - cb[0]) as f32 * 1.1, (cb[3] - cb[1]) as f32 * 1.1);
            let (w, h) = (((b[2] - b[0]) as f32 * 1.1).max(cw), ((b[3] - b[1]) as f32 * 1.1).max(ch));
            let (cx, cy) = ((b[0] + b[2]) as f32 / 2.0, (b[1] + b[3]) as f32 / 2.0);
            let s = (IMAGE_SIZE / LOW_RES) as f32;
            let clamp = |v: f32| v.clamp(0.0, LOW_RES as f32) * s;
            let corners = [
                HeadPoint { x: clamp(cx - w / 2.0), y: clamp(cy - h / 2.0), label: 2 },
                HeadPoint { x: clamp(cx + w / 2.0), y: clamp(cy + h / 2.0), label: 3 },
            ];
            // on the frame alone (no memory): the memory is what lost the changed part
            let alone = self.conditioned(&feats, frame, true, reverse)?;
            let again = self.heads.forward(&alone, &feats.s0, &feats.s1, &corners, None, false, None)?;
            let (aa, _) = area_box(&again.mask);
            let covered = again.mask.iter().zip(&out.mask).filter(|(a, o)| **a > 0.0 && **o > 0.0).count();
            // keep it when it holds what was tracked and is about the prompted size
            if again.object_score > 0.0 && covered as f32 >= ANCHOR_COVER * oa as f32 && (aa as f32) <= ca as f32 * 1.25 && aa > oa {
                out = again;
                self.last_candidates.push((-1.0, aa as f32 / ca.max(1) as f32));
            }
        }
        // the memory is made from the decoder's mask before the holes are filled, as the reference
        let mem = self.encode_memory(&feats, &out.mask, out.object_score, false)?;
        let mut mask = out.mask.clone();
        fill_holes(&mut mask, LOW_RES, FILL_HOLE_AREA);
        let result = Self::output(&out, &mask);
        self.frames.insert(frame, FrameOut { cond: false, mem, obj_ptr: out.obj_ptr, mask, iou: out.iou, score: out.object_score });
        self.prune(frame);
        Ok(result)
    }

    fn reset(&mut self) {
        self.frames.clear();
        self.cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_holes_are_filled_big_ones_and_the_outside_kept() {
        let side = 16;
        let mut m = vec![5.0f32; side * side];
        // a 2 × 2 hole and a 3 × 4 hole
        for (x, y) in [(3, 3), (4, 3), (3, 4), (4, 4)] {
            m[y * side + x] = -1.0;
        }
        for y in 8..11 {
            for x in 8..12 {
                m[y * side + x] = -1.0;
            }
        }
        fill_holes(&mut m, side, 8);
        assert_eq!(m[3 * side + 3], 0.1);
        assert_eq!(m[9 * side + 9], -1.0);
        fill_holes(&mut [0.0; 3], 16, 8);
    }

    /// A compute that returns `bad` results on demand.
    struct Fake {
        short_features: bool,
        nan_memory: bool,
    }

    impl Sam2Compute for Fake {
        fn name(&self) -> String {
            "fake".into()
        }
        fn image(&self, _: &[f32]) -> Result<Sam2Features, MaskError> {
            let n = if self.short_features { 7 } else { EMBED_TOKENS * D_MODEL };
            Ok(Sam2Features { s0: vec![0.0; 256 * 256 * 32], s1: vec![0.0; 128 * 128 * 64], feat: vec![0.0; n] })
        }
        fn memory_attention(&self, feat: &[f32], _: &[f32], _: &[f32], _: usize) -> Result<Vec<f32>, MaskError> {
            Ok(feat.to_vec())
        }
        fn memory_encoder(&self, _: &[f32], _: &[f32]) -> Result<Vec<f32>, MaskError> {
            Ok(vec![if self.nan_memory { f32::NAN } else { 0.0 }; EMBED_TOKENS * MEM_DIM])
        }
    }

    /// Prompts before anything, empty or NaN prompts, and a device returning bad sizes or
    /// non-finite values are errors, never panics (all-zero weights: no checkpoint needed).
    #[test]
    fn hostile_use_and_bad_devices_are_errors() {
        use crate::Point;
        use crate::sam2::weights::{Sam2Weights, expected};
        use crate::sam2::{Sam2Config, Sam2Size};
        use candle_core::{DType, Device, Tensor};
        let cfg = Sam2Config::builtin(Sam2Size::BasePlus);
        let t = expected(&cfg).into_iter().map(|(n, s)| (n, Tensor::zeros(s.as_slice(), DType::F32, &Device::Cpu).unwrap())).collect();
        let heads = Arc::new(Sam2Heads::new(Arc::new(Sam2Weights::from_tensors(cfg, t).unwrap())).unwrap());
        let img = RgbImage::new(32, 18, vec![100; 32 * 18 * 3]).unwrap();
        let click = Prompt { points: vec![Point { x: 0.5, y: 0.5, positive: true }], ..Prompt::default() };
        let ok = Arc::new(Fake { short_features: false, nan_memory: false });
        let mut tr = Sam2Tracker::new(ok, heads.clone(), 0).unwrap();
        assert!(matches!(tr.propagate(3, &img, Direction::Forward), Err(MaskError::BadPrompt(_))));
        assert!(matches!(tr.add_prompt(0, &img, &Prompt::default()), Err(MaskError::BadPrompt(_))));
        let nan = Prompt { points: vec![Point { x: f32::NAN, y: 0.5, positive: true }], ..Prompt::default() };
        assert!(matches!(tr.add_prompt(0, &img, &nan), Err(MaskError::BadPrompt(_))));
        // zero weights: nothing visible, but every path runs (prompt, forward, backward, a fix)
        assert!(tr.add_prompt(5, &img, &click).is_ok());
        assert!(tr.propagate(6, &img, Direction::Forward).is_ok());
        assert!(tr.propagate(4, &img, Direction::Backward).is_ok());
        assert!(tr.add_prompt(6, &img, &click).is_ok());
        assert_eq!(tr.stored(6).map(|s| s.0), Some(true));
        assert!(tr.propagate(usize::MAX, &img, Direction::Forward).is_ok());
        tr.reset();
        assert!(tr.propagate(6, &img, Direction::Forward).is_err());
        let short = Arc::new(Fake { short_features: true, nan_memory: false });
        let mut tr = Sam2Tracker::new(short, heads.clone(), 10).unwrap();
        assert!(matches!(tr.add_prompt(0, &img, &click), Err(MaskError::Failed(_))));
        let nanmem = Arc::new(Fake { short_features: false, nan_memory: true });
        let mut tr = Sam2Tracker::new(nanmem, heads, 10).unwrap();
        assert!(matches!(tr.add_prompt(0, &img, &click), Err(MaskError::Failed(_))));
    }

    /// A square on a plain background, clicked once and followed for two frames with the CPU
    /// reference (Base+; skips when the checkpoint isn't installed).
    #[test]
    fn a_clicked_square_is_found_and_followed_on_the_cpu() {
        use crate::sam2::{Sam2Config, Sam2Size, cpu::Sam2Cpu, weights::Sam2Weights};
        use crate::{Matte, Point};
        let Some((pt, yaml)) = crate::sam2::tests_support::files(Sam2Size::BasePlus) else { return };
        let cfg = Sam2Config::from_yaml(&std::fs::read_to_string(yaml).unwrap()).unwrap();
        let w = Arc::new(Sam2Weights::load(&pt, cfg).unwrap());
        let cpu: Arc<dyn Sam2Compute> = Arc::new(Sam2Cpu::new(w.clone()).unwrap());
        let heads = Arc::new(Sam2Heads::new(w).unwrap());
        let mut tr = Sam2Tracker::new(cpu, heads, 3).unwrap();
        let (fw, fh) = (320, 180);
        let frame = |k: usize| {
            let inside = move |x: usize, y: usize| (100 + 12 * k..180 + 12 * k).contains(&x) && (50..130).contains(&y);
            let mut d = Vec::with_capacity(fw * fh * 3);
            for y in 0..fh {
                for x in 0..fw {
                    d.extend_from_slice(if inside(x, y) { &[220, 60, 40] } else { &[40, 90, 160] });
                }
            }
            let truth = Matte { width: fw, height: fh, alpha: (0..fw * fh).map(|i| if inside(i % fw, i / fw) { 255 } else { 0 }).collect() };
            (RgbImage::new(fw, fh, d).unwrap(), truth)
        };
        let (img, truth) = frame(0);
        let t = std::time::Instant::now();
        let m = tr.add_prompt(0, &img, &Prompt { points: vec![Point { x: 140.0 / 320.0, y: 0.5, positive: true }], ..Prompt::default() }).unwrap();
        let iou = m.segment.matte(fw, fh).unwrap().iou(&truth).unwrap();
        eprintln!("prompt: IoU {iou:.3}, {:.1} s", t.elapsed().as_secs_f64());
        assert!(m.visible && iou > 0.9, "IoU {iou}");
        for k in 1..3 {
            let (img, truth) = frame(k);
            let t = std::time::Instant::now();
            let m = tr.propagate(k, &img, Direction::Forward).unwrap();
            let iou = m.segment.matte(fw, fh).unwrap().iou(&truth).unwrap();
            eprintln!("frame {k}: IoU {iou:.3}, {:.1} s", t.elapsed().as_secs_f64());
            assert!(m.visible && iou > 0.85, "frame {k}: IoU {iou}");
        }
    }
}
