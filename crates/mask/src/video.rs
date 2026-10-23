//! Video segmentation: prompt a subject on one or more frames, then follow it frame by frame
//! forward or backward, with fix strokes on any frame that the follower then remembers.
//!
//! - [`VideoSegmenter`]: the interface Magic Mask drives (fork G3.4).
//! - [`crate::sam2::tracker::Sam2Tracker`] (feature `sam`): SAM 2.1 with its memory of earlier
//!   frames, the main implementation.
//! - [`ImageTracker`]: any image [`Segmenter`] (MobileSAM on the CPU) plus the colour-guided
//!   follow of [`crate::track`]: the fallback. It has no memory: a fix stroke restarts the
//!   follow from that frame.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::track::{FollowOptions, Track, follow};
use crate::{MaskError, Prompt, RgbImage, Segment, Segmenter};

/// Which way propagation runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Forward,
    Backward,
}

/// One frame's result.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoMask {
    pub segment: Segment,
    /// The model thinks the subject is on the frame (SAM 2's object score > 0). When `false` the
    /// segment is empty.
    pub visible: bool,
}

/// A promptable video segmenter that follows one subject.
pub trait VideoSegmenter: Send {
    /// For logs and the UI: which model and where it runs.
    fn name(&self) -> String;
    /// A user prompt (stroke / click / box) on `frame` showing `image`. The first prompt starts
    /// the subject; a prompt on a frame that was already followed is a fix stroke: it corrects
    /// that frame and enters the memory, so later frames follow the correction.
    fn add_prompt(&mut self, frame: usize, image: &RgbImage, prompt: &Prompt) -> Result<VideoMask, MaskError>;
    /// Follow the subject onto `frame` (normally one step from the last frame done in
    /// `direction`). Needs a prompt first ([`MaskError::BadPrompt`] otherwise).
    fn propagate(&mut self, frame: usize, image: &RgbImage, direction: Direction) -> Result<VideoMask, MaskError>;
    /// Forget every prompt and result (a new subject).
    fn reset(&mut self);
}

/// An image [`Segmenter`] driven frame by frame with [`crate::track::follow`].
pub struct ImageTracker {
    model: Arc<dyn Segmenter>,
    options: FollowOptions,
    /// The prompted frames' tracks.
    prompts: BTreeMap<usize, Track>,
    /// The follow state per direction: (last frame, its track).
    state: [Option<(usize, Track)>; 2],
}

impl ImageTracker {
    pub fn new(model: Arc<dyn Segmenter>) -> Self {
        Self { model, options: FollowOptions::default(), prompts: BTreeMap::new(), state: [None, None] }
    }

    fn slot(d: Direction) -> usize {
        match d {
            Direction::Forward => 0,
            Direction::Backward => 1,
        }
    }
}

impl VideoSegmenter for ImageTracker {
    fn name(&self) -> String {
        "MobileSAM + colour follow".into()
    }

    fn add_prompt(&mut self, frame: usize, image: &RgbImage, prompt: &Prompt) -> Result<VideoMask, MaskError> {
        let e = self.model.embed(image)?;
        let seg = self.model.segment(&e, prompt)?;
        let visible = seg.logits.iter().any(|v| *v > 0.0);
        if let Some(t) = Track::start(seg.clone(), image) {
            self.prompts.insert(frame, t.clone());
            // the follow restarts from the newest prompt in both directions
            self.state = [Some((frame, t.clone())), Some((frame, t))];
        }
        Ok(VideoMask { segment: seg, visible })
    }

    fn propagate(&mut self, frame: usize, image: &RgbImage, direction: Direction) -> Result<VideoMask, MaskError> {
        if let Some(t) = self.prompts.get(&frame) {
            let seg = t.segment().clone();
            self.state[Self::slot(direction)] = Some((frame, t.clone()));
            return Ok(VideoMask { segment: seg, visible: true });
        }
        let slot = Self::slot(direction);
        let start = match self.state.get(slot).cloned().flatten() {
            Some(s) => s,
            None => {
                // nearest prompt behind `frame` in this direction
                let p = match direction {
                    Direction::Forward => self.prompts.range(..frame).next_back(),
                    Direction::Backward => self.prompts.range(frame..).next(),
                };
                let (f, t) = p.ok_or_else(|| MaskError::BadPrompt("stroke the subject on a frame first".into()))?;
                (*f, t.clone())
            }
        };
        let e = self.model.embed(image)?;
        match follow(self.model.as_ref(), &e, image, &start.1, &self.options)? {
            Some((t, _)) => {
                let seg = t.segment().clone();
                if let Some(s) = self.state.get_mut(slot) {
                    *s = Some((frame, t));
                }
                Ok(VideoMask { segment: seg, visible: true })
            }
            // lost: hold the last good mask, say it's not visible
            None => Ok(VideoMask { segment: start.1.segment().clone(), visible: false }),
        }
    }

    fn reset(&mut self) {
        self.prompts.clear();
        self.state = [None, None];
    }
}
