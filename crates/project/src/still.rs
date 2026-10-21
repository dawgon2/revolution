//! Gallery stills (fork GRADE-PLAN G1.5): a frame grabbed from the Program monitor, the grade
//! that made it and where it came from. The picture itself is a PNG outside the project file
//! (named by [`Still::file_name`]); the grade and the source time are enough to re-render it.

use crate::grade::Grade;
use crate::{ClipId, ItemId};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

/// The most stills a project keeps.
pub const MAX_STILLS: usize = 1000;
/// The most characters of a still label.
pub const MAX_STILL_LABEL: usize = 64;
/// The widest a still's picture is stored, in pixels.
pub const MAX_STILL_WIDTH: u32 = 1920;

/// One gallery still.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Still {
    /// Unique within the project (never reused while the still exists).
    pub id: u64,
    #[serde(default)]
    pub label: String,
    /// The clip the frame came from (it may have been deleted since) and its name then.
    #[serde(default)]
    pub clip: Option<ClipId>,
    #[serde(default)]
    pub clip_name: String,
    /// The sequence and the sequence time of the grab, so the picture can be re-rendered.
    #[serde(default)]
    pub sequence: Option<ItemId>,
    #[serde(default)]
    pub time: Tick,
    /// The grade the clip had (None = ungraded).
    #[serde(default)]
    pub grade: Option<Grade>,
    /// Size of the stored picture.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// Distinguishes grabs that reuse an id (undo then grab again); only ever grows. Caches of
    /// the picture key on it.
    #[serde(default)]
    pub nonce: u64,
}

impl Still {
    /// The picture's file name inside the stills folder.
    pub fn file_name(&self) -> String {
        format!("still-{}.png", self.id)
    }

    /// A reference picture brought in from a file (`stills.import`): no clip, no sequence and
    /// no grade, only a picture to look at, compare against and match to.
    pub fn is_reference(&self) -> bool {
        self.clip.is_none() && self.sequence.is_none()
    }
}
