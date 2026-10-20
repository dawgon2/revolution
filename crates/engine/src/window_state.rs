//! The main window's size and maximised state, restored at the next start (L1.3).
//!
//! Kept in `<state dir>/window.json` (`$XDG_STATE_HOME/filmcraft` on a default Linux install, the
//! data folder elsewhere; [`crate::dirs`]): it is state FilmCraft remembers, not a setting. Only
//! the size and whether the window was maximised: Wayland compositors place windows themselves,
//! so a position could not be restored there anyway. A missing, unreadable or nonsensical file
//! means the default size.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File name inside the state directory.
pub const FILE: &str = "window.json";
/// The window's default inner size in points (the size of a first start).
pub const DEFAULT_SIZE: [f32; 2] = [1600.0, 980.0];
/// The window's size when the app opens into the Project Manager (fork, owner 2026-10-20, FORK-PLAN
/// §1a.108: never full screen or maximised there, like Resolve's; the editor's own size and
/// maximised state come back when a project opens).
pub const PROJECT_MANAGER_SIZE: [f32; 2] = [1360.0, 800.0];
/// The smallest inner size the window allows, in points.
pub const MIN_SIZE: [f32; 2] = [900.0, 560.0];
/// Larger sizes in the file are clamped to this (no display is wider; guards a corrupt file).
pub const MAX_SIDE: f32 = 16384.0;
/// A real `window.json` is under 100 bytes; anything larger is not ours and is not read.
const MAX_FILE_BYTES: u64 = 4096;

/// What is remembered about the window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    /// Inner size in points of the window when it is not maximised.
    pub inner_size: [f32; 2],
    /// Whether the window was maximised when FilmCraft closed.
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self { inner_size: DEFAULT_SIZE, maximized: false }
    }
}

impl WindowState {
    /// `window.json` in the state directory `state_dir`.
    pub fn path(state_dir: &Path) -> PathBuf {
        state_dir.join(FILE)
    }

    /// The state saved at `path`, sanitised; `None` when there is none or it can't be read.
    pub fn load(path: &Path) -> Option<Self> {
        let len = std::fs::metadata(path).ok()?.len();
        if len > MAX_FILE_BYTES {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<Self>(&text).ok()?.sanitized()
    }

    /// Write the state to `path` (creating its folder), replacing the old file in one rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let s = self.sanitized().unwrap_or_default();
        let text = serde_json::to_string(&s).map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// The state with its size clamped to [`MIN_SIZE`] … [`MAX_SIDE`]; `None` when a side isn't
    /// a finite number.
    pub fn sanitized(self) -> Option<Self> {
        let [w, h] = self.inner_size;
        if !w.is_finite() || !h.is_finite() {
            return None;
        }
        Some(Self { inner_size: [w.clamp(MIN_SIZE[0], MAX_SIDE), h.clamp(MIN_SIZE[1], MAX_SIDE)], maximized: self.maximized })
    }
}

/// Follows the window frame by frame and keeps what to save when it closes.
///
/// A maximised, full-screen or minimised window keeps the size it had before, so un-maximising
/// after the next start returns to the size the user chose. A compositor may report the
/// maximised size one frame before it reports the window maximised, so a size is held back for a
/// frame and dropped if the next frame is maximised at that same size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowTracker {
    state: WindowState,
    /// The last non-maximised frame's size, not yet confirmed by the frame after it.
    pending: Option<[f32; 2]>,
}

impl WindowTracker {
    /// Start from the state the window was opened with.
    pub fn new(start: WindowState) -> Self {
        Self { state: start, pending: None }
    }

    /// One frame's view of the window: its inner size in points and the maximised / full-screen /
    /// minimised flags the windowing system reported (`None` = not known).
    pub fn observe(&mut self, inner_size: Option<[f32; 2]>, maximized: Option<bool>, fullscreen: Option<bool>, minimized: Option<bool>) {
        let size = inner_size.and_then(|s| WindowState { inner_size: s, maximized: false }.sanitized()).map(|s| s.inner_size);
        if fullscreen == Some(true) || minimized == Some(true) {
            self.pending = None;
            return;
        }
        match maximized {
            Some(true) => {
                self.state.maximized = true;
                // the frame before was this same size: it was the maximised window already
                if let Some(p) = self.pending.take()
                    && size != Some(p)
                {
                    self.state.inner_size = p;
                }
            }
            Some(false) => {
                self.state.maximized = false;
                if let Some(p) = self.pending.take() {
                    self.state.inner_size = p;
                }
                self.pending = size;
            }
            // unknown (some X11 window managers): only the size is followed
            None => {
                if let Some(p) = self.pending.take() {
                    self.state.inner_size = p;
                }
                if let Some(size) = size
                    && !self.state.maximized
                {
                    self.state.inner_size = size;
                }
            }
        }
    }

    /// What to save.
    pub fn state(&self) -> WindowState {
        match self.pending {
            Some(p) if !self.state.maximized => WindowState { inner_size: p, maximized: false },
            _ => self.state,
        }
    }
}
