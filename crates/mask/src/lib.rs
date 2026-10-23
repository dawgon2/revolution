//! FilmCraft Magic Mask (fork GRADE-PLAN G3): promptable subject segmentation for grade nodes.
//!
//! - [`Segmenter`]: encode a frame once ([`Segmenter::embed`]), then turn prompts (clicked /
//!   stroked points, a box, the previous frame's mask) into a mask ([`Segmenter::segment`]).
//!   Propagation feeds each frame's result back as the next frame's [`Prompt::prior`] plus its
//!   bounding box, so the model follows the subject without new clicks.
//! - [`Segment`] holds the model's low-resolution logits; [`Segment::matte`] scales them to any
//!   frame size as an 8-bit matte.
//! - [`refine`]: grow / shrink, clean black / white, soften, invert a matte.
//! - `sam` (feature `sam`): MobileSAM (Apache-2.0) on the CPU via candle.
//!
//! Every number that comes from a project, a prompt or a model file is treated as hostile:
//! sizes are capped, coordinates clamped, NaNs refused.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::any::Any;

pub mod refine;
#[cfg(feature = "sam")]
pub mod sam;
pub mod sam2;
#[cfg(feature = "sam")]
#[doc(hidden)]
pub mod tinyvit_ref;
pub mod track;
pub mod video;

/// Side of the square low-resolution mask grid the model works on (SAM: 256 × 256).
pub const LOW_RES: usize = 256;

/// Largest frame side accepted (larger frames are refused, not allocated).
pub const MAX_SIDE: usize = 16_384;

/// Most prompt points accepted in one request.
pub const MAX_POINTS: usize = 64;

/// [`Prompt::prefer_whole`] (Magic Mask's Person mode): among the decoder's candidates for an
/// ambiguous prompt, the largest one whose predicted IoU is at most this far below the best one
/// is kept. SAM scores a good whole figure and its shirt within a few hundredths of each other
/// (around 0.85–0.98), while a candidate that spilt into the background scores clearly lower;
/// 0.1 takes the whole figure over its parts without taking such a spill.
pub const WHOLE_MARGIN: f32 = 0.1;

/// Why a mask could not be made. Every variant says why in plain language.
#[derive(Clone, Debug, PartialEq)]
pub enum MaskError {
    /// The model file is not there.
    ModelMissing(String),
    /// The model file is there but could not be read as the expected model.
    ModelCorrupt(String),
    /// The frame has an impossible size or wrong data length.
    BadImage(String),
    /// A prompt had no usable point / box, or NaN coordinates.
    BadPrompt(String),
    /// The embedding was made by a different segmenter.
    WrongEmbedding,
    /// The model failed while running.
    Failed(String),
}

impl std::fmt::Display for MaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaskError::ModelMissing(p) => write!(f, "the Magic Mask model is not installed ({p})"),
            MaskError::ModelCorrupt(e) => write!(f, "the Magic Mask model file is damaged or not the expected model: {e}"),
            MaskError::BadImage(e) => write!(f, "the frame can't be used for Magic Mask: {e}"),
            MaskError::BadPrompt(e) => write!(f, "the Magic Mask stroke can't be used: {e}"),
            MaskError::WrongEmbedding => write!(f, "the frame was prepared by a different Magic Mask model"),
            MaskError::Failed(e) => write!(f, "Magic Mask failed: {e}"),
        }
    }
}

impl std::error::Error for MaskError {}

/// An 8-bit RGB frame, rows top to bottom, `width * height * 3` bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbImage {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl RgbImage {
    /// Checks the size (both sides 1 ..= [`MAX_SIDE`]) and the data length.
    pub fn new(width: usize, height: usize, data: Vec<u8>) -> Result<Self, MaskError> {
        check_size(width, height)?;
        let want = width.checked_mul(height).and_then(|n| n.checked_mul(3));
        if want != Some(data.len()) {
            return Err(MaskError::BadImage(format!("{width}×{height} needs {want:?} bytes, got {}", data.len())));
        }
        Ok(Self { width, height, data })
    }
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

fn check_size(width: usize, height: usize) -> Result<(), MaskError> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(MaskError::BadImage(format!("size {width}×{height} (1 to {MAX_SIDE} per side)")));
    }
    Ok(())
}

/// A clicked / stroked point, in frame coordinates normalised to 0..1 (x right, y down):
/// `x = pixel column / width` (the pixel's left edge, as SAM counts; no half-pixel offset).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    /// `true`: part of the subject; `false`: not part of it.
    pub positive: bool,
}

/// A box around the subject, normalised to 0..1 (`x0 <= x1`, `y0 <= y1` after [`Rect::sorted`]);
/// the far edges are exclusive (a sub-pixel difference from SAM's inclusive boxes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// Corners in order and clamped to the frame; `None` when a coordinate is not finite.
    pub fn sorted(self) -> Option<Rect> {
        let v = [self.x0, self.y0, self.x1, self.y1];
        if v.iter().any(|c| !c.is_finite()) {
            return None;
        }
        let c = |a: f32| a.clamp(0.0, 1.0);
        Some(Rect { x0: c(self.x0.min(self.x1)), y0: c(self.y0.min(self.y1)), x1: c(self.x0.max(self.x1)), y1: c(self.y0.max(self.y1)) })
    }
}

/// What the segmenter is asked to find on one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prompt {
    pub points: Vec<Point>,
    pub rect: Option<Rect>,
    /// The previous frame's mask (propagation), in the same model's low-resolution grid.
    pub prior: Option<Segment>,
    /// Ask for the whole figure (Magic Mask's Person mode): when the prompt leaves the extent
    /// open (points only: one click on a shirt could mean the shirt or the person), keep the
    /// largest candidate within [`WHOLE_MARGIN`] of the best-scored one instead of the
    /// best-scored one. No effect with a box or a previous mask.
    pub prefer_whole: bool,
}

impl Prompt {
    /// Points with finite coordinates clamped to the frame, at most [`MAX_POINTS`]; refuses a
    /// prompt with nothing usable (no point, no box, no prior).
    pub fn checked(&self) -> Result<(Vec<Point>, Option<Rect>), MaskError> {
        let points: Vec<Point> = self
            .points
            .iter()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .take(MAX_POINTS)
            .map(|p| Point { x: p.x.clamp(0.0, 1.0), y: p.y.clamp(0.0, 1.0), positive: p.positive })
            .collect();
        let rect = self.rect.and_then(Rect::sorted);
        if points.is_empty() && rect.is_none() && self.prior.is_none() {
            return Err(MaskError::BadPrompt("no point, box or previous mask".into()));
        }
        Ok((points, rect))
    }
}

/// A frame prepared by a segmenter (for MobileSAM: the image encoder's output). Opaque: only
/// the segmenter that made it can use it.
pub struct Embedding {
    width: usize,
    height: usize,
    inner: Box<dyn Any + Send + Sync>,
}

impl Embedding {
    pub fn new(width: usize, height: usize, inner: Box<dyn Any + Send + Sync>) -> Self {
        Self { width, height, inner }
    }
    /// The frame size it was made from.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    pub fn inner<T: 'static>(&self) -> Result<&T, MaskError> {
        self.inner.downcast_ref::<T>().ok_or(MaskError::WrongEmbedding)
    }
}

/// A mask in the model's [`LOW_RES`] × [`LOW_RES`] grid of logits (> 0 = subject).
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// Row-major logits, `LOW_RES * LOW_RES` of them.
    pub logits: Vec<f32>,
    /// The part of the grid the frame covers (0..1 per axis; SAM pads the short side).
    pub cover: (f32, f32),
    /// The model's own quality estimate (predicted IoU, 0..1).
    pub score: f32,
}

impl Segment {
    /// A blank segment (`cover` clamped to 0..1).
    pub fn empty(cover: (f32, f32)) -> Self {
        Self { logits: vec![-20.0; LOW_RES * LOW_RES], cover: (cover.0.clamp(0.0, 1.0), cover.1.clamp(0.0, 1.0)), score: 0.0 }
    }

    /// The logit at grid position (`gx`, `gy`) in grid pixels, bilinear, clamped at the edges.
    fn sample(&self, gx: f32, gy: f32) -> f32 {
        let max = (LOW_RES - 1) as f32;
        let gx = gx.clamp(0.0, max);
        let gy = gy.clamp(0.0, max);
        let x0 = gx.floor() as usize;
        let y0 = gy.floor() as usize;
        let x1 = (x0 + 1).min(LOW_RES - 1);
        let y1 = (y0 + 1).min(LOW_RES - 1);
        let fx = gx - x0 as f32;
        let fy = gy - y0 as f32;
        let at = |x: usize, y: usize| self.logits.get(y * LOW_RES + x).copied().unwrap_or(-20.0);
        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// The mask scaled to a `width` × `height` frame: 255 where the bilinear logit is > 0. A
    /// cover that is not finite or not positive covers nothing (an empty matte).
    pub fn matte(&self, width: usize, height: usize) -> Result<Matte, MaskError> {
        check_size(width, height)?;
        let mut alpha = Vec::new();
        alpha.try_reserve_exact(width * height).map_err(|_| MaskError::BadImage(format!("no memory for a {width}×{height} matte")))?;
        let (cx, cy) = self.cover;
        if !(cx.is_finite() && cy.is_finite() && cx > 0.0 && cy > 0.0) {
            alpha.resize(width * height, 0);
            return Ok(Matte { width, height, alpha });
        }
        let gw = cx.min(1.0) * LOW_RES as f32;
        let gh = cy.min(1.0) * LOW_RES as f32;
        let sx = gw / width as f32;
        let sy = gh / height as f32;
        for y in 0..height {
            let gy = (y as f32 + 0.5) * sy - 0.5;
            for x in 0..width {
                let gx = (x as f32 + 0.5) * sx - 0.5;
                alpha.push(if self.sample(gx, gy) > 0.0 { 255 } else { 0 });
            }
        }
        Ok(Matte { width, height, alpha })
    }
}

/// An 8-bit matte (0 = outside, 255 = inside), row-major.
#[derive(Clone, Debug, PartialEq)]
pub struct Matte {
    pub width: usize,
    pub height: usize,
    pub alpha: Vec<u8>,
}

impl Matte {
    /// Intersection over union of the inside (alpha ≥ 128) of two mattes of the same size;
    /// `None` for different sizes; 1 when both are empty.
    pub fn iou(&self, other: &Matte) -> Option<f32> {
        if self.width != other.width || self.height != other.height || self.alpha.len() != other.alpha.len() {
            return None;
        }
        let (mut inter, mut union) = (0u64, 0u64);
        for (a, b) in self.alpha.iter().zip(&other.alpha) {
            let (a, b) = (*a >= 128, *b >= 128);
            inter += u64::from(a && b);
            union += u64::from(a || b);
        }
        Some(if union == 0 { 1.0 } else { inter as f32 / union as f32 })
    }

    /// The normalised bounding box of the inside, or `None` when it is empty.
    pub fn bounds(&self) -> Option<Rect> {
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, a) in self.alpha.iter().enumerate() {
            if *a >= 128 {
                let (x, y) = (i % self.width.max(1), i / self.width.max(1));
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        if x0 == usize::MAX || self.width == 0 || self.height == 0 {
            return None;
        }
        let (w, h) = (self.width as f32, self.height as f32);
        Some(Rect { x0: x0 as f32 / w, y0: y0 as f32 / h, x1: (x1 + 1) as f32 / w, y1: (y1 + 1) as f32 / h })
    }
}

/// Which candidate to keep, from each one's (predicted IoU, area in grid pixels): the
/// best-scored (the first of equals), or with `prefer_whole` the largest whose score is within
/// [`WHOLE_MARGIN`] of the best (equal areas: the better score). Non-finite scores never win
/// (all non-finite: the first). `None` without candidates.
pub fn choose_candidate(cands: &[(f32, usize)], prefer_whole: bool) -> Option<usize> {
    let best = cands.iter().enumerate().filter(|(_, c)| c.0.is_finite()).fold(None, |b: Option<(usize, f32)>, (i, c)| match b {
        Some((_, s)) if s >= c.0 => b,
        _ => Some((i, c.0)),
    });
    let Some((mut pick, top)) = best else { return (!cands.is_empty()).then_some(0) };
    if prefer_whole {
        for (i, (s, a)) in cands.iter().enumerate() {
            let Some((ps, pa)) = cands.get(pick).copied() else { continue };
            if s.is_finite() && *s >= top - WHOLE_MARGIN && (*a > pa || (*a == pa && *s > ps)) {
                pick = i;
            }
        }
    }
    Some(pick)
}

/// How a single-image segmenter answers a prompt from its [`Segmenter::candidates`] (MobileSAM's
/// `segment`): an ambiguous prompt (one point; with [`Prompt::prefer_whole`] any points without
/// a box or previous mask) asks for several candidates, then [`choose_candidate`] keeps one.
pub fn segment_by_candidates<S: Segmenter + ?Sized>(model: &S, embedding: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError> {
    let (points, rect) = prompt.checked()?;
    let open = rect.is_none() && prompt.prior.is_none() && !points.is_empty();
    let whole = prompt.prefer_whole && open;
    let multimask = whole || (open && points.len() == 1);
    let all = model.candidates(embedding, prompt, multimask)?;
    let keys: Vec<(f32, usize)> = all.iter().map(|s| (s.score, s.logits.iter().filter(|v| **v > 0.0).count())).collect();
    let i = choose_candidate(&keys, whole).ok_or_else(|| MaskError::Failed("the decoder gave no mask".into()))?;
    all.into_iter().nth(i).ok_or_else(|| MaskError::Failed("the decoder gave no mask".into()))
}

/// A promptable segmentation model.
pub trait Segmenter: Send + Sync {
    /// Prepare a frame (the expensive part, once per frame).
    fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError>;
    /// Find the prompted subject on a prepared frame.
    fn segment(&self, embedding: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError>;
    /// The model's candidate masks for a prompt (one decoder call): several (SAM: whole / part /
    /// sub-part) when `multimask` and the model can, else one. Default: [`Segmenter::segment`].
    fn candidates(&self, embedding: &Embedding, prompt: &Prompt, multimask: bool) -> Result<Vec<Segment>, MaskError> {
        let _ = multimask;
        Ok(vec![self.segment(embedding, prompt)?])
    }
}

/// Propagate a mask from one frame to the next: the next frame's prompt is the previous
/// segment as the prior plus its bounding box, grown by 15 % of its size and at least 4 % of the
/// frame per side (so a small subject moving a few pixels a frame stays inside), so the subject is followed
/// without new clicks. `None` when the previous mask is empty (the subject is lost: the caller
/// keeps the last good mask and asks for a fix stroke).
pub fn follow_prompt(previous: &Segment, frame_size: (usize, usize)) -> Option<Prompt> {
    let (w, h) = frame_size;
    let m = previous.matte(w.clamp(1, 512), h.clamp(1, 512)).ok()?;
    let b = m.bounds()?;
    let gx = ((b.x1 - b.x0) * 0.15).max(0.04);
    let gy = ((b.y1 - b.y0) * 0.15).max(0.04);
    let rect = Rect { x0: b.x0 - gx, y0: b.y0 - gy, x1: b.x1 + gx, y1: b.y1 + gy }.sorted()?;
    Some(Prompt { points: Vec::new(), rect: Some(rect), prior: Some(previous.clone()), prefer_whole: false })
}

#[cfg(test)]
mod tests;
