//! Remembers the main window's size and maximised state between runs (L1.3,
//! `filmcraft_engine::window_state`): read before the window opens, followed every frame,
//! written when FilmCraft closes.

use filmcraft_engine::window_state::{WindowState, WindowTracker};
use std::path::PathBuf;

/// Wraps the app: forwards every [`eframe::App`] method the app implements and follows the
/// window's geometry. Methods the app leaves at their defaults are left at the defaults here.
pub struct WindowMemory<A> {
    inner: A,
    path: PathBuf,
    tracker: WindowTracker,
    /// While this says so, the window isn't followed (the Project Manager's window, fork §1a.108).
    skip: fn(&A) -> bool,
}

impl<A> WindowMemory<A> {
    /// `path` is `window.json` in the state directory; `start` is the editor's state from the last
    /// run; the window isn't followed while `skip` says so.
    pub fn new(inner: A, path: PathBuf, start: WindowState, skip: fn(&A) -> bool) -> Self {
        Self { inner, path, tracker: WindowTracker::new(start), skip }
    }
}

impl<A: eframe::App> eframe::App for WindowMemory<A> {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.inner.raw_input_hook(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let (size, maximized, fullscreen, minimized) = ctx.input(|i| {
            let v = i.viewport();
            // Wayland has no window position, so egui-winit leaves `inner_rect` empty there; the
            // area egui draws into is the same inner size.
            let size = v.inner_rect.unwrap_or_else(|| i.viewport_rect()).size();
            ([size.x, size.y], v.maximized, v.fullscreen, v.minimized)
        });
        if !(self.skip)(&self.inner) {
            self.tracker.observe(Some(size), maximized, fullscreen, minimized);
        }
        self.inner.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.ui(ui, frame);
    }

    fn on_exit(&mut self) {
        if let Err(e) = self.tracker.state().save(&self.path) {
            log::warn!("could not remember the window size in {}: {e}", self.path.display());
        }
        self.inner.on_exit();
    }
}
