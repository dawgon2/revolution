//! The FilmCraft egui frontend.
//!
//! Thin by design: all project changes go through `filmcraft_engine::Session::execute`; this crate
//! owns only presentation state ([`state::UiState`]), GPU textures, the playback clock and the
//! control-channel handlers. Swap it for another toolkit without touching the engine.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

/// A literal UI string in the language the interface is drawn in (`i18n::t`). Only literals: the
/// i18n tests read every `tl!` literal in the crate and require a Spanish translation for it.
macro_rules! tl {
    ($s:literal) => {
        $crate::i18n::t($s)
    };
}

/// A translated template with `{name}` placeholders filled in, for text that `format!` would build.
/// After the template literal come the values: `n = count`, or just `gain` for a variable of the
/// same name. Values are formatted with `Display`; the translation may reorder placeholders.
macro_rules! tlf {
    (@v $k:ident = $v:expr) => {
        $v
    };
    (@v $k:ident) => {
        $k
    };
    ($s:literal $(, $k:ident $(= $v:expr)?)* $(,)?) => {
        $crate::i18n::fmt($crate::i18n::t($s), &[$((stringify!($k), ToString::to_string(&tlf!(@v $k $(= $v)?)).as_str())),*])
    };
}

pub mod app_logo;
pub mod automation;
mod cjk;
pub mod coach_ids;
pub mod control;
pub mod crash;
pub mod credits;
pub mod desktop;
pub mod dock;
mod dock_shared;
mod dock_ui;
pub mod frames;
pub mod graphics_device;
pub mod header;
pub mod i18n;
pub mod icons;
pub mod links;
pub mod menus;
pub mod native_dialogs;
pub mod panels;
pub mod perf;
#[cfg(not(target_arch = "wasm32"))]
pub mod play_ahead;
pub mod scrub;
pub mod shuttle_audio;
pub mod source_playback;
pub mod state;
pub mod theme;
pub mod widgets;
pub mod window_chrome;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use egui::{TextureHandle, TextureOptions};
use filmcraft_engine::{Services, Session};
use filmcraft_time::Tick;
use serde_json::{Value, json};

pub use control::ControlRequest;
use dock::PanelKind;
use frames::{FrameKey, FrameServer, Target};
use native_dialogs::FileDialog;
use state::UiState;
use theme::{ThemeKind, Tokens};

/// Audio output provided by the platform layer (cpal on desktop, WebAudio on web).
pub trait AudioOut {
    /// Start output; `fill(buffer, channels)` is called on the audio thread with interleaved f32.
    fn start(&mut self, fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String>;
    fn stop(&mut self);
    /// Device sample rate.
    fn sample_rate(&self) -> u32;
    /// Device output channels (what `fill` will be called with).
    fn channels(&self) -> usize {
        2
    }
    /// Frames played since `start` (the playback master clock), if the device reports it.
    fn played_frames(&self) -> Option<u64>;
    /// Called every frame while playing, before `played_frames`: restart a stream the device
    /// dropped (a driver error, an unplugged device), keeping the clock and `fill`. A stream that
    /// cannot be restarted reports `played_frames() == None`.
    fn recover(&mut self) {}
    /// Hosts and devices that can be chosen in Settings ▸ Audio Hardware.
    fn devices(&self) -> AudioDevices {
        AudioDevices::default()
    }
    /// Apply Settings ▸ Audio Hardware (device class/output, buffer size, sample rate). Takes
    /// effect at the next `start`; `document_rate` is the sequence sample rate for "Attempt to
    /// force hardware to document sample rate".
    fn configure(&mut self, _hw: &filmcraft_engine::settings::AudioHardwarePrefs, _document_rate: Option<u32>) {}
    /// A hardware note for the status bar — e.g. the default output device could not be opened
    /// and a fallback device is in use, or no output device was found — or None.
    fn note(&self) -> Option<String> {
        None
    }
}

/// What the platform audio layer can open (Settings ▸ Audio Hardware).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AudioDevices {
    /// Audio hosts / device classes (`CoreAudio`, `WASAPI`, `ALSA`…).
    pub hosts: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// Output channels of the selected output device.
    pub output_channels: u16,
}

/// How often the menu is compared with what the host's global menu shows ([`HostHooks::menu_changed`]).
pub const MENU_PUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// Host hooks for native file dialogs etc.
#[derive(Default)]
pub struct HostHooks {
    pub pick_files: Option<Box<dyn FnMut(&[&str]) -> Vec<String>>>,
    /// The cursor in physical screen pixels. Where the windowing layer reports no pointer while files are dragged in from the OS (Windows), a drop is placed where this says it landed.
    pub cursor_screen_position: Option<Box<dyn Fn() -> Option<(i32, i32)>>>,
    pub pick_save: Option<Box<dyn FnMut(&str) -> Option<String>>>,
    pub pick_open_project: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Save dialog with a filter: (filter name, extensions, suggested file name) → path.
    pub pick_save_as: Option<Box<dyn FnMut(&str, &[&str], &str) -> Option<String>>>,
    /// The active keyboard shortcuts changed: update native menu key equivalents.
    pub shortcuts_changed: Option<Box<dyn FnMut(&[menus::MenuItem])>>,
    /// The menu changed (labels, shortcuts, enabled or checked items): the host's global menu
    /// (Plasma, `com.canonical.dbusmenu`) shows the new state. Called at most every
    /// [`MENU_PUBLISH_INTERVAL`], only when something changed.
    pub menu_changed: Option<Box<dyn FnMut(&[menus::MenuItem])>>,
    /// The user's preferred languages (locale tags, most preferred first) for Interface Language ▸
    /// System Language. Without it, System Language is English.
    pub system_languages: Option<Box<dyn Fn() -> Vec<String>>>,
    /// Open dialog for a JSON file (shortcut preset import): filter name, extensions → path.
    pub pick_open_file: Option<Box<dyn FnMut(&str, &[&str]) -> Option<String>>>,
    /// Folder picker (Link Media search, proxy and Project Manager destinations).
    pub pick_folder: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Folder picker starting at the current destination (Export Frame).
    pub pick_folder_at: Option<Box<dyn FnMut(&str) -> Option<String>>>,
    /// Pick one file for a command that relinks to it (Link Media ▸ Locate…, Attach Proxies,
    /// Reconnect Full Resolution) instead of importing it, as [`Self::pick_files`] does. Native
    /// hosts return the path. A host whose picker is asynchronous (the web) returns `None` and
    /// runs `hint.command` with `hint.params` plus `"path"` itself once the user has chosen.
    pub pick_file_for_relink: Option<Box<dyn FnMut(&[&str], Option<RelinkHint>) -> Option<String>>>,
    /// Bring the window on screen for control-channel UI requests *without* taking keyboard focus
    /// (macOS: `orderFrontRegardless`). Without it the app only requests a repaint: it never
    /// activates itself for an agent, because the user's keystrokes would land here.
    pub raise_without_focus: Option<Box<dyn FnMut()>>,
    /// Open a file in its default application, or (`true`) reveal it in the file manager (Edit ▸
    /// Edit Original, Help ▸ Reveal Log Files).
    pub open_path: Option<Box<dyn FnMut(&str, bool) -> Result<(), String>>>,
    /// Open a URL in the system browser (Help menu links, About dialog, Discord button, Import
    /// screen). Without it, or when it fails, links go through `egui::Context::open_url`, which
    /// opens a new tab on the web (the native egui backend is built without its `links` feature).
    pub open_url: Option<Box<dyn FnMut(&str) -> Result<(), String>>>,
    /// The operating system's light or dark appearance when egui cannot report it (Linux desktops
    /// whose Wayland compositor sends no theme to winit). Without it, or without an answer, Auto
    /// uses `egui::Context::system_theme`.
    pub system_theme: Option<SystemThemeFn>,
    /// The desktop's accent colour (Linux: the XDG desktop portal's `accent-color`), for Settings ▸
    /// Appearance ▸ Use the system accent color. Without it, or without an answer, the highlight
    /// colour applies.
    pub system_accent: Option<SystemAccentFn>,
    /// Whether the desktop asks for reduced motion (Linux: the XDG desktop portal, KDE's animation
    /// speed, GNOME's Animations switch), for Settings ▸ Appearance ▸ Animations ▸ Follow the
    /// system. Without it animations stay on.
    pub system_reduced_motion: Option<SystemReducedMotionFn>,
    /// The desktop's text size as a factor of its default (Linux: KDE's General font, GNOME's
    /// `text-scaling-factor`, through the XDG desktop portal), for Settings ▸ Appearance ▸ Match
    /// the desktop's text size. Without it, or without an answer, UI Scale ▸ Auto is the system's
    /// scale alone.
    pub system_text_scale: Option<SystemTextScaleFn>,
    /// Files or an image on the system clipboard, for Paste in the Timeline and the Project panel
    /// (#611). Without it Paste only pastes clips copied in FilmCraft.
    pub clipboard_media: Option<Box<dyn FnMut() -> Option<ClipboardMedia>>>,
    /// Desktop shell integration (taskbar progress, notifications, idle inhibit, single
    /// instance, media keys; [`desktop`]). Without it nothing is published.
    pub desktop: Option<Box<dyn desktop::DesktopHost>>,
    /// Show a file dialog without blocking the UI thread and answer on the returned channel with
    /// the chosen paths (none: cancelled). With it, [`FilmcraftApp::pick`] keeps the frame loop
    /// running while the dialog is open (the desktop app on Linux, #260); without it the
    /// synchronous pickers above are used.
    pub run_dialog: Option<Box<dyn FnMut(native_dialogs::FileDialog) -> Receiver<Vec<String>>>>,
    /// Fork L1.5: [`Self::run_dialog`] with the folder the dialog should start in (the one that
    /// kind of dialog was last used in, or the project's for Save; `None`: the host's default).
    /// Preferred over `run_dialog` when both are set; the chosen folder is remembered.
    pub run_dialog_from: Option<Box<DialogRunnerFrom>>,
    /// This computer's GPUs for Settings ▸ Playback ▸ Graphics device (L2.6; the desktop app:
    /// [`graphics_device::list`]). Without it Settings offers Auto only.
    pub graphics_devices: Option<Box<dyn FnMut() -> Vec<graphics_device::GpuChoice>>>,
    /// Settings ▸ Appearance ▸ App icon changed (fork §1a.108; `rounded` | `square`): the desktop
    /// app sets the window icon and, on Linux, the desktop's launcher icon. Without it nothing
    /// changes outside the app.
    pub app_icon_changed: Option<Box<dyn FnMut(&egui::Context, &str)>>,
}

/// [`HostHooks::run_dialog_from`]: a dialog and the folder it starts in.
pub type DialogRunnerFrom = dyn FnMut(native_dialogs::FileDialog, Option<String>) -> Receiver<Vec<String>>;

/// What [`HostHooks::clipboard_media`] found on the system clipboard.
pub enum ClipboardMedia {
    /// Files copied in the file manager.
    Files(Vec<String>),
    /// An image (a screenshot, an image copied in a browser): 8-bit RGBA, row by row.
    Image { width: usize, height: usize, rgba: Vec<u8> },
}

/// Reads the system appearance (Settings ▸ Appearance ▸ Appearance Mode ▸ Sync with system).
pub type SystemThemeFn = Box<dyn Fn(&egui::Context) -> Option<egui::Theme>>;

/// Reads the desktop's accent colour as 8-bit sRGB (Settings ▸ Appearance ▸ Use the system accent
/// color).
pub type SystemAccentFn = Box<dyn Fn(&egui::Context) -> Option<[u8; 3]>>;

/// Reads whether the desktop asks for reduced motion (Settings ▸ Appearance ▸ Animations).
pub type SystemReducedMotionFn = Box<dyn Fn(&egui::Context) -> bool>;

/// Reads the desktop's text size as a factor of its default size (Settings ▸ Appearance ▸ Match
/// the desktop's text size).
pub type SystemTextScaleFn = Box<dyn Fn(&egui::Context) -> Option<f32>>;

/// The command a [`HostHooks::pick_file_for_relink`] caller runs with the chosen file, for hosts
/// that can only run it later.
#[derive(Clone, Debug)]
pub struct RelinkHint {
    /// `media.relink`, `media.attachProxies` or `media.reconnectFullRes`.
    pub command: String,
    /// Its parameters, without `"path"`.
    pub params: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialog {
    About,
    Shortcuts,
    NewSequence,
    /// Settings (Preferences window; page in `UiState::settings`).
    Preferences,
    /// "Recover unsaved changes from <time>?" (shown at startup when a dead session left some).
    Recovery,
    /// "Are you sure you want to discard your changes?" (File ▸ Revert).
    RevertConfirm,
    /// Clip ▸ Audio Gain… (G).
    AudioGain,
    /// Sequence ▸ Delete Tracks….
    DeleteTracks,
    /// Sequence ▸ Add Tracks….
    AddTracks,
    /// Sequence ▸ Sequence Settings….
    SequenceSettings,
    /// Set Transition Duration (double-click a transition in the Timeline).
    TransitionDuration,
}

#[derive(Default)]
pub struct Playback {
    pub playing: bool,
    pub speed: f64,
    pub looping: bool,
    /// Wall-clock (egui time, s) and timeline tick when playback (re)started.
    anchor_time: f64,
    anchor_tick: Tick,
    /// Audio frames played at anchor (when the audio clock drives).
    pub audio_clock: bool,
    /// The audio clock's last reading and when (egui time, s) it last moved: a device that stops
    /// consuming samples hands the clock back to the wall clock (see [`AUDIO_STALL_S`]).
    audio_seen: (u64, f64),
    /// Audio underruns for the current (or last) play (desktop: sound is mixed ahead).
    #[cfg(not(target_arch = "wasm32"))]
    pub audio_stats: std::sync::Arc<play_ahead::AudioStats>,
    /// Shown / dropped frame accounting for the current (or last) play.
    pub meter: frames::PlaybackMeter,
    /// Waiting for the first frames before starting the clock: when the wait began (egui time,
    /// s; negative = not yet stamped), and whether the Program monitor has them ready.
    pub preroll: Option<f64>,
    pub preroll_ready: bool,
    /// The window was hidden (occluded/minimized) since the monitor last refreshed.
    pub hidden: bool,
    /// Forward playback stops here (Play In to Out, Play from Playhead to Out Point).
    pub stop_at: Option<Tick>,
}

impl Playback {
    /// Fork A1.2: this play's mixer stopped after repeated panics (desktop; the web build mixes
    /// in its callback).
    pub(crate) fn mixer_stopped(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.audio_stats.mixer_stopped.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(target_arch = "wasm32")]
        false
    }
}

/// How long (s) the audio clock may stand still during playback before the wall clock takes over.
/// An output stream can open and then never call back (ALSA with a busy or misconfigured device,
/// #136); playback must not freeze on it. Devices that are slow to start (Bluetooth) stay well
/// under this.
pub const AUDIO_STALL_S: f64 = 1.0;

/// How long `ui.screenshot` waits for the window to present the frame.
const SCREENSHOT_TIMEOUT_S: f64 = 10.0;
/// How long `ui.screenshot` waits for the UI to settle (monitors showing their exact frames, the
/// timeline zoom animation finished) before capturing what is there.
const SCREENSHOT_SETTLE_MAX_S: f64 = 5.0;

/// Export entries that run from the menus through the save panel: command, filter label,
/// extension.
pub(crate) const EXPORT_SAVE_DIALOGS: [(&str, &str, &str); 7] = [
    ("markers.exportCsv", "Marker report (CSV)", "csv"),
    ("file.exportEdl", "EDL", "edl"),
    ("file.exportFcp7Xml", "Final Cut Pro XML", "xml"),
    ("file.exportFcpxml", "FCPXML", "fcpxml"),
    ("file.exportOtio", "OpenTimelineIO", "otio"),
    ("file.exportAle", "Avid Log Exchange", "ale"),
    ("file.exportSelectionProject", "Revolution Project", "fcproj"),
];

pub struct FilmcraftApp {
    pub session: Session,
    pub ui: UiState,
    pub tokens: Tokens,
    pub frames: Arc<FrameServer>,
    pub playback: Playback,
    pub source_playback: source_playback::SourcePlayback,
    pub audio: Option<Box<dyn AudioOut>>,
    /// Audio during scrubbing (#211).
    pub scrub: scrub::ScrubAudio,
    pub hooks: HostHooks,
    pub dialog: Option<Dialog>,
    pub file_dialogs: panels::file_dialogs::FileDialogState,
    /// Ratings of Assistant answers and the Help improve the Assistant dialog (fork AI5).
    pub share: panels::share_example::ShareState,
    /// Fork UPD: when the open app next asks whether the daily update check is due (egui time).
    pub update_next_auto: f64,
    /// The update window was closed after installing (until restart).
    pub update_closed: bool,
    /// `remote.import` jobs whose clips are imported when they finish (STORAGE-PLAN S2).
    pub pending_remote_imports: Vec<(u64, serde_json::Value)>,
    /// Restart Now: the build to start once this one has closed.
    pub restart_into: Option<std::path::PathBuf>,
    /// The native file dialog the host is showing ([`Self::pick`]).
    native_dialog: Option<native_dialogs::PendingDialog>,
    pub auto: automation::Registry,
    /// Named textures (monitors, thumbnails) with the key they show.
    textures: HashMap<String, (FrameKey, TextureHandle)>,
    control_rx: Option<Receiver<ControlRequest>>,
    /// One explicit panel reveal; consumed by the custom renderer, never repeated per frame.
    pub(crate) pending_panel_raise: Option<PanelKind>,
    /// Explicit automation mode: native widget input and native menu commands are excluded.
    pub(crate) control_input_only: bool,
    /// Backend focus before synthetic input changes egui's effective focus for this frame.
    pub(crate) native_window_focused: Option<bool>,
    /// Modifiers owned by an in-flight control gesture, carried between its input frames.
    synthetic_modifiers: egui::Modifiers,
    /// Requests waiting for the UI to show an element: (request, give-up time).
    deferred: Vec<(ControlRequest, f64)>,
    last_ui_time: f64,
    pub(crate) synthetic: Vec<egui::Event>,
    /// BS.1770 loudness of the programme as it plays, and the next sample position to feed.
    pub(crate) loudness: Option<(filmcraft_audio_dsp::LoudnessMeter, i64)>,
    /// Status message last shown and when it first appeared (messages expire after a few seconds).
    status_seen: (String, f64),
    /// Screenshots waiting for their frame: (token, path, crop, reply, give-up time).
    pending_screenshots: Vec<(u64, Option<String>, Option<[f32; 4]>, Sender<Value>, f64)>,
    queued_screenshots: Vec<(u64, f64, u32)>,
    /// A paused monitor drew a stand-in (nearest cached) picture last frame: its exact frame is
    /// still decoding.
    pub(crate) monitor_inexact: bool,
    /// Per monitor: the frames it asked for on its latest paused passes, oldest first (stand-ins
    /// while the newest renders, see `panels::monitor`).
    pub(crate) monitor_asked: HashMap<String, std::collections::VecDeque<FrameKey>>,
    /// The frame whose picture the Program monitor drew last: the frame due, or its stand-in.
    pub(crate) program_shown: Option<FrameKey>,
    /// Consecutive frames the timeline zoom / scroll has been at rest. `ui.elements` answers from
    /// the frame before the last one, so its timeline rects are final from 2 on.
    pub(crate) timeline_still: u32,
    input_waiters: Vec<Sender<Value>>,
    next_token: u64,
    styled: bool,
    /// A panic in the UI pass, shown in an error window until dismissed (see [`crash`]).
    pub ui_error: Option<String>,
    /// Fault injection for robustness tests: the next UI pass panics.
    #[doc(hidden)]
    pub panic_next_frame: bool,
    /// The next close request quits without asking to save (Save / Don't Save was chosen in the
    /// quit prompt, or the control channel's `app.quit`).
    pub quit_confirmed: bool,
    fonts_ready: bool,
    pub integrated_titlebar: bool,
    /// FilmCraft draws the window's title bar itself (fork; Settings ▸ General ▸ Window title
    /// bar): window buttons and resize edges ([`window_chrome`]), no desktop decorations. Follows
    /// the preference in [`Self::apply_prefs`]; the host opens the window to match.
    pub window_controls: bool,
    /// The window's full-screen state as last seen ([`window_chrome::sync_full_screen`]): a change
    /// the desktop made (a KWin shortcut, leaving through the window menu) updates Settings ▸
    /// General ▸ Full screen, so the setting always says what the window does.
    pub shown_full_screen: Option<bool>,
    /// The full-screen state last sent to the window ([`window_chrome::sync_full_screen`]).
    pub sent_full_screen: Option<bool>,
    /// When (egui time) the last full-screen request went to the window.
    pub full_screen_sent_at: Option<f64>,
    /// Tell the window on the first frame whether to be full screen (the desktop app opens it
    /// right already; an agent's window stays windowed: `false`).
    pub full_screen_at_start: bool,
    /// The editor's window (size, maximised) to bring back once the Project Manager the app
    /// opened into closes ([`Self::launch_project_manager_windowed`]).
    pub editor_window: Option<filmcraft_engine::window_state::WindowState>,
    /// When the editor's window was given back ([`window_chrome::restore_editor_window`]); the
    /// window's size isn't remembered until it has settled ([`Self::window_is_editors`]).
    pub editor_window_restored: Option<web_time::Instant>,
    /// Set by the host while the desktop shows this window's menu in its global menu (Plasma's
    /// Global Menu); with Settings ▸ General ▸ Menu bar ▸ Auto the window then hides its own
    /// menu bar, except in full screen ([`Self::in_window_menu`]).
    pub global_menu: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// The menu last given to [`HostHooks::menu_changed`] and when it was checked.
    published_menu: (Vec<menus::MenuItem>, Option<web_time::Instant>),
    pub last_timeline_width: f32,
    /// The sequence whose view `ui.timeline` holds, and that view as it was last exchanged with
    /// `session.state.timeline_views` (see `sync_timeline_view`).
    timeline_view_of: Option<filmcraft_engine::project::ItemId>,
    /// Items imported by an OS file drop, to place on the Timeline at the drop point this frame.
    pending_timeline_drop: Option<(Vec<filmcraft_engine::project::ItemId>, egui::Pos2)>,
    /// Files being dragged over the window from the OS (count, pointer): the Timeline previews where they would land.
    file_drag_hover: Option<(usize, egui::Pos2)>,
    timeline_view_last: Option<filmcraft_engine::project::SequenceView>,
    pub fps: f32,
    last_time: f64,
    bindings: Vec<menus::KeyBinding>,
    /// Shortcut-set revision `bindings` (and the native menu) were built from.
    bindings_rev: u64,
    /// Keyboard Shortcuts dialog state.
    pub shortcut_editor: panels::shortcuts_dialog::EditorState,
    pub toast: Option<(String, f64)>,
    pub tl: panels::timeline::TlState,
    /// The Cut page's two timeline views (fork RS4).
    pub cut: panels::cut_page::CutState,
    /// Commands from outside the UI (native menu bar), invoked on the UI thread.
    pub command_inbox: Option<Receiver<String>>,
    /// A V key press (or a Ctrl+V paste) reached the app and its release hasn't yet.
    v_down: bool,
    /// GPU compositor (when running on wgpu): device state + compositor + the egui texture it feeds.
    pub gpu: Option<GpuState>,
    /// Preview render job being watched (job id, where to start playing when it completes).
    watched_render: Option<(u64, Tick)>,
    /// Jobs seen running, to tell the desktop host once when each finishes ([`desktop`]).
    desktop_jobs: desktop::JobWatch,
    /// Settings last applied to the UI (theme, tooltips, frame cache, audio device).
    applied_prefs: Option<filmcraft_engine::autosave::Preferences>,
    /// Settings ▸ Appearance ▸ UI Scale last applied, with the system scale it was applied for
    /// and the zoom Match the desktop's text size added.
    applied_ui_scale: Option<(Option<f32>, f32, Option<f32>)>,
    /// The system accent colour the theme was last set with (`None` while it isn't followed).
    shown_system_accent: Option<[u8; 3]>,
    /// Whether the style was last set with animations off (Settings ▸ Appearance ▸ Animations).
    shown_reduced_motion: bool,
    workspace_restored: bool,
    /// Window ▸ Workspaces: saved layouts ([`dock::WORKSPACES_FILE`] in the data directory).
    pub workspaces: dock::WorkspacePrefs,
    /// Workspace names and the current one, as last handed to the native menu.
    menu_workspaces: (Vec<String>, String),
    /// The Project Manager home screen's listing, poster textures and poster renders (fork RS2).
    pub(crate) home_rt: panels::home::Runtime,
    /// The workflow the layout was last arranged for (fork RS3, `panels::pages::follow_workflow`).
    pub(crate) layout_workflow: Option<bool>,
    /// Each workflow's layout as it was left at a workflow switch (Premiere, Resolve): workspace
    /// name, dock and floating panels, put back on return so unsaved changes survive.
    pub(crate) layouts_by_workflow: (Option<WorkflowLayout>, Option<WorkflowLayout>),
}

/// A workflow's layout kept across a workflow switch (fork RS3).
pub(crate) type WorkflowLayout = (String, dock::DockNode, Vec<dock::Floating>);

pub struct GpuState {
    pub render_state: eframe::egui_wgpu::RenderState,
    pub compositor: filmcraft_gpu::GpuCompositor,
    pub texture: Option<egui::TextureId>,
    pub last_key: Option<FrameKey>,
    pub size: (u32, u32),
    /// Composite time of the last frame (ms).
    pub last_ms: f32,
    /// Set by the device's uncaptured-error handler: the GPU path failed (validation, lost
    /// device, out of memory). The app then drops it and composites on the CPU.
    pub broken: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Largest texture side the device accepts.
    pub max_texture: u32,
    /// Scopes from the Program monitor's picture (fork Phase 4.4): when the scopes panel last
    /// asked for them, the read-back in flight, and the latest samples.
    pub scopes_wanted: Option<web_time::Instant>,
    pub scope_pending: Option<(FrameKey, filmcraft_gpu::scopes::ScopeReadback)>,
    pub scope_signal: Option<(FrameKey, Arc<filmcraft_scopes::Signal>)>,
    /// Scope samples read back so far (`perf.stats` `scopes.gpuSamples`).
    pub scope_samples: u64,
    /// The last picture was composited with Maximum Render Quality.
    pub last_hq: bool,
}

impl GpuState {
    /// Pick up a finished scope read-back.
    pub fn poll_scopes(&mut self) {
        if let Some((key, r)) = &self.scope_pending
            && let Some(px) = r.try_take()
        {
            let (w, h) = (r.w as usize, r.h as usize);
            self.scope_signal = Some((*key, Arc::new(filmcraft_scopes::Signal::from_rgba8_max(w, h, &px, w, h))));
            self.scope_pending = None;
            self.scope_samples += 1;
        }
    }
}

/// Largest texture side a plan needs on the GPU (output and every layer).
fn plan_side(plan: &filmcraft_render::plan::FramePlan) -> usize {
    plan.max_side()
}

/// A software rasterizer (llvmpipe, SwiftShader, softpipe — VMs and containers without GPU device
/// access, missing or broken drivers) runs wgpu entirely on the CPU. Compositing through one is
/// slower than the native CPU compositor and hides the cause: GPU usage stays at 0 % while the
/// CPU saturates. Returns a description when the adapter is one.
pub fn software_rasterizer(info: &eframe::wgpu::AdapterInfo) -> Option<String> {
    rasterizer_reason(&info.name, info.device_type)
}

fn rasterizer_reason(name: &str, device_type: eframe::wgpu::DeviceType) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if device_type == eframe::wgpu::DeviceType::Cpu || ["llvmpipe", "swiftshader", "softpipe"].iter().any(|s| lower.contains(s)) {
        return Some(format!("software rasterizer ({name}, {device_type:?})"));
    }
    None
}

/// Whether the GPU compositor can run on this adapter: it renders and blends Rgba16Float
/// targets and uploads whole frames as textures. Older or OpenGL-backed GPUs (some Intel Macs,
/// VMs, Linux without Vulkan, WebGL) can't; those use the CPU compositor, which renders the same
/// frames. Returns the reason when unsupported.
pub fn gpu_compositor_unsupported(adapter: &eframe::wgpu::Adapter) -> Option<String> {
    use eframe::wgpu::{TextureFormat, TextureFormatFeatureFlags as F, TextureUsages as U};
    if let Some(why) = software_rasterizer(&adapter.get_info()) {
        return Some(why);
    }
    let f = adapter.get_texture_format_features(TextureFormat::Rgba16Float);
    if !f.allowed_usages.contains(U::RENDER_ATTACHMENT | U::TEXTURE_BINDING) {
        return Some("Rgba16Float is not renderable".into());
    }
    if !f.flags.contains(F::BLENDABLE | F::FILTERABLE) {
        return Some("Rgba16Float is not blendable/filterable".into());
    }
    let max = adapter.limits().max_texture_dimension_2d;
    if max < 4096 {
        return Some(format!("max texture size {max} < 4096"));
    }
    None
}

static WINDOW_GPU_LOST: std::sync::OnceLock<Box<dyn Fn(&str) + Send + Sync>> = std::sync::OnceLock::new();

/// What the app does when the window's GPU device is lost (once; later calls are ignored): the
/// desktop app restarts itself and recovers the session from its journal, because egui-wgpu
/// panics drawing on a lost device (fork XPS sign-off: the Arc ran out of memory mid-playback).
pub fn set_window_gpu_lost_hook(hook: Box<dyn Fn(&str) + Send + Sync>) {
    let _ = WINDOW_GPU_LOST.set(hook);
}

/// The wgpu setup for the app window: eframe's defaults plus the device features the GPU
/// compositor uses when the adapter has them (16-bit normalized textures: 10-bit video uploads
/// without a CPU conversion). On Linux with Vulkan, FilmCraft creates the device itself with
/// DMA-BUF import, so hardware-decoded frames are drawn where the decoder wrote them
/// (`filmcraft_gpu::interop`, docs/adr/0002-linux-video-interop.md); if that fails, eframe's own
/// device setup is used. Pass as `NativeOptions::wgpu_options`.
pub fn wgpu_configuration() -> eframe::egui_wgpu::WgpuConfiguration {
    wgpu_configuration_on("")
}

/// [`wgpu_configuration`] on the GPU Settings ▸ Playback ▸ Graphics device names
/// (`playback.graphicsDevice`, L2.6; "" or a GPU that isn't there: Auto).
pub fn wgpu_configuration_on(graphics_device: &str) -> eframe::egui_wgpu::WgpuConfiguration {
    let mut c = eframe::egui_wgpu::WgpuConfiguration::default();
    #[cfg(target_os = "linux")]
    if let Some(existing) = interop_setup(graphics_device) {
        c.wgpu_setup = eframe::egui_wgpu::WgpuSetup::Existing(existing);
        return c;
    }
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(n) = &mut c.wgpu_setup
        && (graphics_device::preference(graphics_device) != filmcraft_gpu::adapter_pick::GpuPref::Auto
            || filmcraft_gpu::adapter_pick::driver_override().is_set())
    {
        let pref = graphics_device.to_string();
        n.native_adapter_selector = Some(std::sync::Arc::new(move |adapters: &[eframe::wgpu::Adapter], surface: Option<&eframe::wgpu::Surface<'_>>| {
            // only adapters that can show the window; the platform's own backend (DX12, Metal)
            // before Vulkan, as eframe's default would
            let mut usable: Vec<&eframe::wgpu::Adapter> = adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).collect();
            usable.sort_by_key(|a| a.get_info().backend == eframe::wgpu::Backend::Vulkan);
            let infos: Vec<_> = usable.iter().map(|a| a.get_info()).collect();
            let index = graphics_device::pick(&infos, &pref);
            index.and_then(|i| usable.get(i).map(|a| (*a).clone())).ok_or_else(|| "no GPU adapter".to_string())
        }));
    }
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(n) = &mut c.wgpu_setup {
        let base = n.device_descriptor.clone();
        n.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut d = base(adapter);
            d.required_features |= adapter.features() & filmcraft_gpu::wanted_features();
            d
        });
    }
    c
}

/// The device descriptor eframe would use on `adapter`, plus the compositor's features.
pub fn device_descriptor(adapter: &eframe::wgpu::Adapter) -> eframe::wgpu::DeviceDescriptor<'static> {
    eframe::wgpu::DeviceDescriptor {
        label: Some("filmcraft device"),
        required_features: adapter.features() & filmcraft_gpu::wanted_features(),
        // as egui-wgpu: textures large enough for 4K+ surfaces
        required_limits: eframe::wgpu::Limits { max_texture_dimension_2d: 8192, ..eframe::wgpu::Limits::default() },
        ..Default::default()
    }
}

/// A Vulkan instance, the adapter Settings ▸ Playback ▸ Graphics device names (else the
/// high-performance one) and a device with DMA-BUF import (Linux).
#[cfg(target_os = "linux")]
fn interop_setup(graphics_device: &str) -> Option<eframe::egui_wgpu::WgpuSetupExisting> {
    use eframe::wgpu;
    if std::env::var_os("FILMCRAFT_NO_GPU_IMPORT").is_some() {
        return None;
    }
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    // Auto with `WGPU_POWER_PREF` set: wgpu's own choice; otherwise the shared rule (fork XPS a)
    let chosen = if graphics_device::preference(graphics_device) == filmcraft_gpu::adapter_pick::GpuPref::Auto && wgpu::PowerPreference::from_env().is_some() {
        None
    } else {
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        let infos: Vec<_> = adapters.iter().map(|a| a.get_info()).collect();
        graphics_device::pick(&infos, graphics_device).and_then(|i| adapters.into_iter().nth(i))
    };
    let adapter = match chosen {
        Some(a) => a,
        None => {
            let power = wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance);
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: power, ..Default::default() })).ok()?
        }
    };
    match filmcraft_gpu::interop::create_device(&adapter, &device_descriptor(&adapter))? {
        Ok((device, queue)) => {
            log::info!("GPU: {} with DMA-BUF import (hardware-decoded frames drawn in place)", adapter.get_info().name);
            // on an NVIDIA GPU, NVDEC's pictures stay on it (fork 8.6c): CUDA copies them into
            // memory this device shares
            if adapter.get_info().vendor == 0x10de {
                filmcraft_gpu::interop::set_cuda_target(Some(device.clone()));
            }
            Some(eframe::egui_wgpu::WgpuSetupExisting { instance, adapter, device, queue })
        }
        Err(e) => {
            log::warn!("GPU device with DMA-BUF import failed ({e}); using the default device");
            None
        }
    }
}

impl FilmcraftApp {
    /// Enable the GPU compositor on the eframe wgpu device.
    pub fn set_wgpu(&mut self, rs: eframe::egui_wgpu::RenderState) {
        if let Some(why) = gpu_compositor_unsupported(&rs.adapter) {
            log::warn!("GPU compositor disabled ({why}); compositing on the CPU");
            // Say so instead of letting the preview quietly crawl: "GPU 0 %, CPU 100 %" with no
            // explanation read like a renderer bug when the machine simply has no GPU access.
            self.ui.status = format!("{} ({why})", tl!("GPU acceleration is unavailable; preview renders on the CPU"));
            return;
        }
        // wgpu's default reaction to a validation error or lost device is to panic, which closed
        // the app (or left the monitors blank) on GPUs the compositor doesn't suit. Log instead and
        // fall back to the CPU compositor.
        let broken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = broken.clone();
        rs.device.on_uncaptured_error(std::sync::Arc::new(move |e: eframe::wgpu::Error| {
            log::error!("GPU error, switching to the CPU compositor: {e}");
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }));
        // a user shader effect running when the GPU stops is quarantined (ADR 0006)
        if let Some(dir) = filmcraft_engine::user_effects::folder(&self.session) {
            filmcraft_gpu::set_user_effect_lost_hook(Box::new(move |keys| {
                if let Err(e) = filmcraft_engine::user_effects::quarantine(&dir, keys) {
                    log::error!("couldn't quarantine user effects {keys:?}: {e}");
                }
            }));
        }
        rs.device.set_device_lost_callback(|reason, msg| {
            log::error!("GPU device lost ({reason:?}): {msg}");
            filmcraft_gpu::notify_device_lost();
            // the window's renderer (egui-wgpu) can't draw on a lost device and panicked a moment
            // later (XPS 16, the Arc out of memory): the app restarts first (`set_window_gpu_lost_hook`)
            if let Some(hook) = WINDOW_GPU_LOST.get() {
                hook(&format!("{reason:?}: {msg}"));
            }
        });
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| filmcraft_gpu::GpuCompositor::new(&rs.device, &rs.queue)));
        match made {
            Ok(compositor) if !broken.load(std::sync::atomic::Ordering::Relaxed) => {
                let max_texture = rs.device.limits().max_texture_dimension_2d;
                self.gpu = Some(GpuState {
                    render_state: rs,
                    compositor,
                    texture: None,
                    last_key: None,
                    size: (0, 0),
                    last_ms: 0.0,
                    broken,
                    max_texture,
                    scopes_wanted: None,
                    scope_pending: None,
                    scope_signal: None,
                    scope_samples: 0,
                    last_hq: false,
                });
            }
            _ => log::error!("GPU compositor failed to initialise; compositing on the CPU"),
        }
    }

    /// Drop the GPU compositor after a GPU error; the monitors composite on the CPU from then on.
    fn disable_gpu(&mut self, why: &str) {
        log::error!("GPU compositor disabled: {why}");
        if let Some(g) = self.gpu.take()
            && let Some(id) = g.texture
        {
            g.render_state.renderer.write().free_texture(&id);
        }
        self.ui.status = tl!("Graphics problem: switched to software compositing").into();
    }

    /// Composite a plan on the GPU and return the egui texture showing it.
    pub fn gpu_present(&mut self, key: FrameKey, plan: &frames::GpuPlan) -> Option<(egui::TextureId, (u32, u32))> {
        if self.gpu.as_ref()?.broken.load(std::sync::atomic::Ordering::Relaxed) {
            self.disable_gpu("device error");
            return None;
        }
        // Maximum Render Quality (Lanczos minification) for a paused picture; playback keeps
        // the cheaper supersampling
        let hq = !self.playback.playing && self.session.active_sequence().is_some_and(|q| q.settings.max_render_quality);
        let g = self.gpu.as_mut()?;
        g.poll_scopes();
        if g.last_key == Some(key)
            && g.last_hq == hq
            && let Some(t) = g.texture
        {
            return Some((t, g.size));
        }
        if plan_side(&plan.plan) > g.max_texture as usize {
            self.disable_gpu("frame larger than the GPU's maximum texture size");
            return None;
        }
        let t0 = web_time::Instant::now();
        g.compositor.set_high_quality(hq);
        g.last_hq = hq;
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (view, size) = g.compositor.composite_prepared(&plan.plan, Some(&plan.prepared));
            (view.clone(), size)
        }));
        let Ok((view, size)) = ran else {
            self.disable_gpu("compositor panicked");
            return None;
        };
        let g = self.gpu.as_mut()?;
        g.last_ms = t0.elapsed().as_secs_f32() * 1000.0;
        // a user shader effect turned off by the slow-pass rule (ADR 0006)
        if let Some(m) = g.compositor.take_user_effect_messages().pop() {
            self.ui.status = m;
        }
        // scopes open: sample this picture for them (one read-back in flight at a time)
        if g.scope_pending.is_none() && g.scopes_wanted.is_some_and(|t| t.elapsed().as_secs_f32() < 0.5) {
            g.scope_pending = g.compositor.request_scope_samples(filmcraft_scopes::MAX_W as u32, filmcraft_scopes::MAX_H as u32).map(|r| (key, r));
        }
        let mut renderer = g.render_state.renderer.write();
        let id = match g.texture {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(&g.render_state.device, &view, eframe::wgpu::FilterMode::Linear, id);
                id
            }
            None => renderer.register_native_texture(&g.render_state.device, &view, eframe::wgpu::FilterMode::Linear),
        };
        g.texture = Some(id);
        g.last_key = Some(key);
        g.size = size;
        Some((id, size))
    }
}

impl FilmcraftApp {
    pub fn new(mut session: Session) -> Self {
        session.shortcuts.register_external(menus::external_commands());
        let recovery = !session.recovery_candidates().is_empty();
        let frames = Arc::new(FrameServer::new(session.media.clone(), session.services.clone(), session.previews.clone(), FrameServer::default_workers()));
        let workspaces =
            session.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| dock::WorkspacePrefs::load(&d.join(dock::WORKSPACES_FILE))).unwrap_or_default();
        let language = i18n::Language::parse(&session.prefs.general.interface_language).unwrap_or_default();
        // "Ask the coach" checks the controls it names against what this UI has shown
        let auto = automation::Registry::default();
        session.coach_ids = Some(Arc::new(coach_ids::UiIds(auto.seen.clone())));
        Self {
            session,
            ui: UiState { language, ..UiState::default() },
            tokens: Tokens::for_kind(ThemeKind::Dark),
            frames,
            playback: Playback { speed: 1.0, ..Default::default() },
            source_playback: Default::default(),
            audio: None,
            scrub: Default::default(),
            hooks: HostHooks::default(),
            // Unsaved changes left by a session that died are offered first thing.
            dialog: recovery.then_some(Dialog::Recovery),
            file_dialogs: Default::default(),
            share: Default::default(),
            update_next_auto: 0.0,
            update_closed: false,
            pending_remote_imports: Vec::new(),
            restart_into: None,
            native_dialog: None,
            auto,
            textures: HashMap::new(),
            control_rx: None,
            pending_panel_raise: None,
            control_input_only: false,
            native_window_focused: None,
            synthetic_modifiers: egui::Modifiers::NONE,
            deferred: Vec::new(),
            last_ui_time: 0.0,
            synthetic: Vec::new(),
            loudness: None,
            status_seen: (String::new(), 0.0),
            pending_screenshots: Vec::new(),
            queued_screenshots: Vec::new(),
            monitor_inexact: false,
            monitor_asked: HashMap::new(),
            program_shown: None,
            timeline_still: 0,
            input_waiters: Vec::new(),
            next_token: 1,
            styled: false,
            ui_error: None,
            panic_next_frame: false,
            quit_confirmed: false,
            fonts_ready: false,
            integrated_titlebar: false,
            window_controls: false,
            shown_full_screen: None,
            sent_full_screen: None,
            full_screen_sent_at: None,
            full_screen_at_start: true,
            editor_window: None,
            editor_window_restored: None,
            global_menu: None,
            published_menu: (Vec::new(), None),
            last_timeline_width: 1000.0,
            timeline_view_of: None,
            pending_timeline_drop: None,
            file_drag_hover: None,
            timeline_view_last: None,
            fps: 60.0,
            last_time: 0.0,
            bindings: Vec::new(),
            bindings_rev: 0,
            shortcut_editor: Default::default(),
            toast: None,
            tl: Default::default(),
            cut: Default::default(),
            command_inbox: None,
            v_down: false,
            gpu: None,
            watched_render: None,
            desktop_jobs: Default::default(),
            applied_prefs: None,
            applied_ui_scale: None,
            shown_system_accent: None,
            shown_reduced_motion: false,
            workspace_restored: false,
            workspaces,
            menu_workspaces: Default::default(),
            home_rt: Default::default(),
            layout_workflow: None,
            layouts_by_workflow: (None, None),
        }
    }

    /// The second half of the launch ([`launch_project`]; fork RS2): show the Project Manager home
    /// screen when that was the startup choice, nothing else was opened (media files on the
    /// command line, a recovered project) and the app isn't run by a test harness.
    pub fn finish_launch(&mut self, action: Option<filmcraft_engine::projects::StartupAction>, media_given: bool) {
        if action == Some(filmcraft_engine::projects::StartupAction::ProjectManager) && !media_given && !filmcraft_engine::projects::project_open(&self.session)
        {
            panels::home::show_home(self);
        }
    }

    /// The app opened into the Project Manager in a plain window (fork, owner 2026-10-20, FORK-PLAN
    /// §1a.108): never full screen there at launch (Project Manager full screen lasts until the
    /// next start), and the editor's `editor` window comes back when a project opens
    /// ([`window_chrome::restore_editor_window`]).
    pub fn launch_project_manager_windowed(&mut self, editor: filmcraft_engine::window_state::WindowState) {
        self.editor_window = Some(editor);
        if self.session.prefs.general.project_manager_full_screen {
            let mut p = self.session.prefs.clone();
            p.general.project_manager_full_screen = false;
            if let Err(e) = self.session.set_prefs(p) {
                log::warn!("saving preferences: {e}");
            }
        }
    }

    /// Whether the window now shows the editor at its own size (not the Project Manager, and not
    /// the moments after the editor's size was given back, when the window still reports the
    /// Project Manager's): the window size the next start restores is taken only then.
    pub fn window_is_editors(&self) -> bool {
        !self.ui.home.open && self.editor_window.is_none() && self.editor_window_restored.is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(1500))
    }

    /// Open the Project Manager home screen (`projects.show`).
    pub fn show_project_manager(&mut self) {
        panels::home::show_home(self);
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    /// Isolate an automation-owned app from native keyboard, pointer, drop and menu input.
    /// Window close/quit and screenshot delivery remain available.
    pub fn with_control_input_only(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_input_only = true;
        self.with_control(rx)
    }

    pub fn services(&self) -> Arc<dyn Services> {
        self.session.services.clone()
    }

    /// Rebuild the frame server if the session's media pool was replaced (e.g. project opened).
    fn sync_pool(&mut self) {
        if !Arc::ptr_eq(&self.frames.pool, &self.session.media) || !Arc::ptr_eq(&self.frames.previews, &self.session.previews) {
            self.frames = Arc::new(FrameServer::new(
                self.session.media.clone(),
                self.session.services.clone(),
                self.session.previews.clone(),
                FrameServer::default_workers(),
            ));
            self.textures.clear();
            self.tl.reset_media_caches();
            // the new frame server needs the Memory settings
            self.applied_prefs = None;
        }
    }

    /// Show theme `k` with the Settings ▸ Appearance highlight colour and contrast.
    pub fn set_theme(&mut self, ctx: &egui::Context, k: ThemeKind) {
        let a = &self.session.prefs.appearance;
        self.shown_system_accent = if a.follow_system_accent { self.system_accent(ctx) } else { None };
        let highlight = self.highlight(ctx, &a.highlight_color, a.follow_system_accent);
        self.tokens = Tokens::for_kind(k).with_appearance(highlight, a.accessible_contrast);
        theme::apply_visuals(ctx, &self.tokens);
        self.apply_motion(ctx);
        self.apply_tooltips(ctx);
        self.ui.dark = k != ThemeKind::Light;
    }

    /// The system appearance: the host's reading, else what the windowing toolkit reports.
    pub fn system_theme(&self, ctx: &egui::Context) -> Option<egui::Theme> {
        self.hooks.system_theme.as_ref().and_then(|read| read(ctx)).or_else(|| ctx.system_theme())
    }

    /// Whether animations are off now (Settings ▸ Appearance ▸ Animations, following the desktop
    /// when it is `system`).
    pub fn reduced_motion(&self, ctx: &egui::Context) -> bool {
        let system = self.hooks.system_reduced_motion.as_ref().is_some_and(|read| read(ctx));
        self.session.prefs.appearance.reduced_motion(system)
    }

    /// Switch egui's animations (menus, collapsing sections, scrolling) off or back on. Runs after
    /// [`theme::apply_visuals`], which sets the normal animation time.
    fn apply_motion(&mut self, ctx: &egui::Context) {
        let reduced = self.reduced_motion(ctx);
        self.shown_reduced_motion = reduced;
        ctx.global_style_mut(|s| {
            if reduced {
                s.animation_time = 0.0;
                s.scroll_animation = egui::style::ScrollAnimation::none();
            } else {
                s.scroll_animation = egui::style::ScrollAnimation::default();
            }
        });
    }

    /// The desktop's accent colour, if the host reads one.
    pub fn system_accent(&self, ctx: &egui::Context) -> Option<[u8; 3]> {
        self.hooks.system_accent.as_ref().and_then(|read| read(ctx))
    }

    /// The highlight colour to show: the system accent when `follow_system` is on and the desktop
    /// reports one, else the Highlight Color `pref` (`#rrggbb`).
    pub fn highlight(&self, ctx: &egui::Context, pref: &str, follow_system: bool) -> Option<[u8; 3]> {
        follow_system.then(|| self.system_accent(ctx)).flatten().or_else(|| filmcraft_engine::settings::parse_hex(pref))
    }

    /// The theme Settings ▸ Appearance selects: the light or dark theme of the Appearance Mode,
    /// Auto following the system (dark when it gives no answer).
    pub fn appearance_kind(&self, ctx: &egui::Context) -> ThemeKind {
        let light = self.system_theme(ctx).map(|t| t == egui::Theme::Light);
        ThemeKind::from_pref(self.session.prefs.appearance.shown_theme(light))
    }

    fn apply_tooltips(&self, ctx: &egui::Context) {
        // Settings ▸ General ▸ Show Tool Tips
        let delay = if self.session.prefs.general.show_tool_tips { 0.5 } else { 1.0e9 };
        ctx.global_style_mut(|s| s.interaction.tooltip_delay = delay);
    }

    /// Settings ▸ Appearance ▸ UI Scale: a fixed scale makes a point that many physical pixels,
    /// whatever scale the system reports. It is applied only when the setting or the system scale
    /// changes, so Ctrl+- / Ctrl++ still zoom the interface for the session; going back to Auto
    /// restores the system's scale. With Match the desktop's text size, Auto is the system's
    /// scale times the desktop's text factor, followed while it changes.
    fn apply_ui_scale(&mut self, ctx: &egui::Context) {
        let appearance = &self.session.prefs.appearance;
        let want = appearance.ui_scale_factor();
        let text = appearance.text_scale_zoom(self.hooks.system_text_scale.as_ref().and_then(|read| read(ctx)));
        let native = ctx.native_pixels_per_point().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
        if self.applied_ui_scale == Some((want, native, text)) {
            return;
        }
        let prev = self.applied_ui_scale.replace((want, native, text));
        match (want, text) {
            (Some(scale), _) => ctx.set_zoom_factor(scale / native),
            (None, Some(factor)) => ctx.set_zoom_factor(factor),
            (None, None) if prev.is_some_and(|(p, _, t)| p.is_some() || t.is_some()) => ctx.set_zoom_factor(1.0),
            (None, None) => {}
        }
    }

    /// Make the UI follow the settings after they change (theme, tooltips, frame cache budget,
    /// play after rendering, audio device).
    pub fn apply_prefs(&mut self, ctx: &egui::Context) {
        self.apply_ui_scale(ctx);
        filmcraft_engine::remote::apply_prefs(&self.session.prefs.remote);
        // Appearance Mode ▸ Sync with system: follow the system while running. The host reports
        // changes as they happen (no polling) and wakes the UI only when the value changes.
        // The same for Use the system accent color and Animations ▸ Sync with system.
        if self.applied_prefs.is_some() {
            let k = self.appearance_kind(ctx);
            let accent_changed = self.session.prefs.appearance.follow_system_accent && self.system_accent(ctx) != self.shown_system_accent;
            let motion_changed = self.reduced_motion(ctx) != self.shown_reduced_motion;
            if k != self.tokens.kind || accent_changed || motion_changed {
                self.set_theme(ctx, k);
            }
        }
        window_chrome::restore_editor_window(self, ctx);
        window_chrome::sync_full_screen(self, ctx);
        if self.applied_prefs.as_ref() == Some(&self.session.prefs) {
            return;
        }
        let p = self.session.prefs.clone();
        let prev = self.applied_prefs.take();
        // Window title bar: FilmCraft's own or the desktop's (also on the first apply, in case the
        // host opened the window the other way)
        let own = window_chrome::wants_own(&p.general.title_bar);
        if prev.is_none() || own != self.window_controls {
            self.window_controls = own;
            if window_chrome::SUPPORTED {
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!own));
            }
        }
        if let Some(q) = &prev
            && q.appearance.app_icon != p.appearance.app_icon
            && let Some(f) = self.hooks.app_icon_changed.as_mut()
        {
            f(ctx, &p.appearance.app_icon);
        }
        if prev.as_ref().is_none_or(|q| q.appearance != p.appearance || q.general.show_tool_tips != p.general.show_tool_tips) {
            let k = self.appearance_kind(ctx);
            self.set_theme(ctx, k);
        }
        // The workflow is part of the condition: its terminology overlay relabels the native menu too.
        if prev.as_ref().is_none_or(|q| q.general.interface_language != p.general.interface_language || q.workflow != p.workflow) {
            let language = match i18n::Language::parse(&p.general.interface_language) {
                Some(l) => l,
                None if p.general.interface_language == "system" => {
                    i18n::Language::from_locales(&self.hooks.system_languages.as_ref().map(|f| f()).unwrap_or_default())
                }
                None => i18n::Language::default(),
            };
            if language == i18n::Language::Ja && !i18n::install_japanese_font(ctx) {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Japanese font is installed on this system; the interface stays in English").into();
            } else if language == i18n::Language::ZhCn && !i18n::chinese_font_available() {
                self.ui.language = i18n::Language::En;
                self.ui.status = tl!("no Chinese font is installed on this system; the interface stays in English").into();
            } else {
                self.ui.language = language;
            }
            i18n::set_current(self.ui.language);
            i18n::terms::set_resolve(self.resolve_workflow());
            let items = menus::menu_items(self);
            if let Some(hook) = self.hooks.shortcuts_changed.as_mut() {
                hook(&items);
            }
        }
        self.frames.set_cache_budget(p.memory.frame_cache_mb as usize * (1 << 20));
        self.ui.play_after_render = p.timeline.play_after_rendering;
        if prev.as_ref().is_none_or(|q| q.audio_hardware != p.audio_hardware) {
            if let Some(input) = self.session.voiceover.input.as_mut() {
                input.configure_host(&p.audio_hardware.device_class);
            }
            let rate = self.session.active_sequence().map(|q| q.settings.sample_rate);
            let playing = self.playback.playing && self.playback.preroll.is_none();
            if let Some(a) = self.audio.as_mut() {
                a.stop();
                a.configure(&p.audio_hardware, rate);
            }
            if playing {
                self.playback.anchor_tick = self.session.playhead();
                self.playback.anchor_time = ctx.input(|i| i.time);
                self.start_audio();
            }
        }
        if let Some(note) = self.audio.as_ref().and_then(|a| a.note()) {
            self.ui.status = note;
        }
        if prev.is_none() && !self.workspace_restored {
            // reopen the workspace in use when the app last closed
            self.workspace_restored = true;
            if let Some(w) = dock::find(&self.workspaces, &self.workspaces.current) {
                self.set_workspace(&w);
            }
        }
        self.applied_prefs = Some(p);
    }

    /// Every sequence has its own Timeline view (zoom, scroll, track heights), as Premiere's
    /// sequence tabs do. `ui.timeline` holds the active sequence's; the views of all sequences are
    /// in `session.state.timeline_views`, which is saved with the project. Each frame this
    /// - gives `ui.timeline` the view of a sequence that has just become active (a sequence shown
    ///   for the first time is fitted, with default track heights),
    /// - writes a change made in the panel to the session,
    /// - and takes over a view that was changed in the session (by a command or the control
    ///   channel).
    fn sync_timeline_view(&mut self) {
        let Some(active) = self.session.state.active_sequence else {
            self.timeline_view_of = None;
            return;
        };
        let stored = self.session.state.timeline_views.get(&active).copied();
        let v = &mut self.ui.timeline;
        let shown = filmcraft_engine::project::SequenceView {
            pps: v.target_pps,
            scroll: v.target_scroll,
            v_scroll: v.v_scroll,
            a_scroll: v.a_scroll,
            video_track_h: v.video_track_h,
            audio_track_h: v.audio_track_h,
        };
        let show = |v: &mut state::TimelineView, s: filmcraft_engine::project::SequenceView| {
            (v.pps, v.target_pps, v.scroll, v.target_scroll) = (s.pps, s.pps, s.scroll, s.scroll);
            (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (s.v_scroll, s.a_scroll, s.video_track_h, s.audio_track_h);
            (v.fit_pending, v.fit_empty) = (false, None);
        };
        if self.timeline_view_of != Some(active) {
            self.timeline_view_of = Some(active);
            match stored.and_then(|s| s.checked()) {
                Some(s) => {
                    show(v, s);
                    self.timeline_view_last = Some(s);
                }
                None => {
                    let d = state::TimelineView::default();
                    (v.v_scroll, v.a_scroll, v.video_track_h, v.audio_track_h) = (0.0, 0.0, d.video_track_h, d.audio_track_h);
                    (v.fit_pending, v.fit_empty) = (true, None);
                    self.timeline_view_last = None;
                }
            }
        } else if v.fit_pending {
            // the panel has not fitted the sequence yet: nothing to keep
        } else if self.timeline_view_last != Some(shown) {
            self.session.state.timeline_views.insert(active, shown);
            self.timeline_view_last = Some(shown);
        } else if let Some(s) = stored.filter(|s| *s != shown).and_then(|s| s.checked()) {
            show(v, s);
            self.timeline_view_last = Some(s);
            self.session.state.timeline_views.insert(active, s);
        }
    }

    fn workspaces_path(&self) -> Option<std::path::PathBuf> {
        self.session.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join(dock::WORKSPACES_FILE))
    }

    /// Replace the saved workspaces and write them to the data directory.
    pub fn set_workspaces(&mut self, w: dock::WorkspacePrefs) -> Result<(), String> {
        self.workspaces = w;
        match self.workspaces_path() {
            Some(p) => self.workspaces.save(&p).map_err(|e| format!("saving workspaces: {e}")),
            None => Ok(()),
        }
    }

    /// Settings ▸ General ▸ Workflow is DaVinci Resolve.
    /// Whether an automation id exists: shown this session, or built at run time by a known
    /// pattern ("Ask the coach" drops steps naming any other).
    pub fn known_ui_id(&self, id: &str) -> bool {
        self.auto.known_ui_id(id)
    }

    pub fn resolve_workflow(&self) -> bool {
        self.session.prefs.workflow == filmcraft_engine::settings::Workflow::Resolve
    }

    /// Whether the window draws its own menu bar: not while the desktop's global menu shows it
    /// (Settings ▸ General ▸ Menu bar ▸ Auto), except in full screen, which covers that panel.
    pub fn in_window_menu(&self) -> bool {
        let global = self.session.prefs.general.menu_bar != "window"
            && !window_chrome::wanted_full_screen(self)
            && self.global_menu.as_ref().is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed));
        self.ui.show_menu_bar && !global
    }

    /// Give the host's global menu the current menu when it changed (checked at most every
    /// [`MENU_PUBLISH_INTERVAL`]; a skipped check asks for a frame when the interval is over).
    fn publish_menu(&mut self, ctx: &egui::Context) {
        if self.hooks.menu_changed.is_none() {
            return;
        }
        let now = web_time::Instant::now();
        if let Some(t) = self.published_menu.1 {
            let since = now.duration_since(t);
            if since < MENU_PUBLISH_INTERVAL {
                // check again once the interval is over, even if nothing else wakes the UI
                // (the pointer is on the desktop's panel after a click there)
                ctx.request_repaint_after(MENU_PUBLISH_INTERVAL.saturating_sub(since));
                return;
            }
        }
        self.published_menu.1 = Some(now);
        // Menu bar ▸ In the window: the global menu stays empty
        let items = if self.session.prefs.general.menu_bar == "window" { Vec::new() } else { menus::menu_items(self) };
        if items != self.published_menu.0 {
            if let Some(hook) = self.hooks.menu_changed.as_mut() {
                hook(&items);
            }
            self.published_menu.0 = items;
        }
    }

    pub fn set_workspace(&mut self, name: &str) {
        // Resolve workflow: the Color workspace is the node grader (GRADE-PLAN G1.8) and the
        // Premiere workspaces a page stands for open that page; the Premiere workflow opens the
        // Premiere workspace for a page's name (fork RS3, `dock::for_workflow`)
        let name = dock::for_workflow(name, self.resolve_workflow()).to_string();
        let name = name.as_str();
        self.ui.workspace = name.to_string();
        let layout = dock::saved_docking(&self.workspaces, name);
        self.ui.dock = layout.root;
        self.ui.floating = layout.floating;
        self.pending_panel_raise = None;
        self.ui.keys.maximized = None;
        if self.workspaces.current != name {
            let mut next = self.workspaces.clone();
            next.current = name.to_string();
            if let Err(e) = self.set_workspaces(next) {
                self.ui.status = e;
            }
        }
        if name == "Color" || name == "Color Grading" {
            self.ui.show_scopes = false;
        }
    }

    pub fn show_panel(&mut self, p: PanelKind) {
        if p == PanelKind::Timeline && !self.ui.floating.iter().any(|group| group.panels.contains(&p)) {
            self.ui.dock.restore_timeline();
        }
        if !self.ui.dock.contains(p) && !self.ui.floating.iter().any(|group| group.panels.contains(&p)) {
            let near = match p {
                // a side panel beside Properties (or Essential Sound), never over the Program monitor
                PanelKind::TextToSpeech => [PanelKind::Properties, PanelKind::EssentialSound, PanelKind::EssentialGraphics]
                    .into_iter()
                    .find(|k| self.ui.dock.contains(*k))
                    .unwrap_or(PanelKind::Program),
                PanelKind::LumetriColor | PanelKind::ColorPrimaries | PanelKind::EssentialGraphics | PanelKind::EssentialSound | PanelKind::Properties => {
                    PanelKind::Program
                }
                PanelKind::Source
                | PanelKind::EffectControls
                | PanelKind::AudioClipMixer
                | PanelKind::Metadata
                | PanelKind::LumetriScopes
                | PanelKind::AudioTrackMixer
                | PanelKind::Text
                | PanelKind::ReferenceMonitor
                | PanelKind::Timecode => PanelKind::Source,
                PanelKind::ColorNodes | PanelKind::Gallery | PanelKind::ColorClips => PanelKind::Timeline,
                _ => PanelKind::Project,
            };
            self.ui.dock.open_near(p, near);
        }
        let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: p });
        self.ui.focused = p;
    }

    /// Reveal in Project: bring the Project panel forward, showing the bin that holds `item` with
    /// the search cleared, so the (already selected) item is on show.
    fn reveal_in_project(&mut self, item: filmcraft_project::ItemId) {
        // the bins on the way down to the item (bounded: a project file can nest bins arbitrarily deep)
        fn path(b: &filmcraft_project::Bin, item: filmcraft_project::ItemId, depth: usize, out: &mut Vec<u64>) -> bool {
            if depth > 256 {
                return false;
            }
            for c in &b.children {
                match c {
                    filmcraft_project::BinEntry::Item(i) if *i == item => return true,
                    filmcraft_project::BinEntry::Bin(inner) => {
                        out.push(inner.id.0);
                        if path(inner, item, depth + 1, out) {
                            return true;
                        }
                        out.pop();
                    }
                    _ => {}
                }
            }
            false
        }
        let mut bins = Vec::new();
        let found = path(&self.session.project.root, item, 0, &mut bins);
        // the list shows the whole tree: open the bins on the way. Icons and freeform show one bin
        // at a time: go into the one that holds the item.
        let list = self.session.prefs.project_panel.view.mode == filmcraft_engine::project_panel::ViewMode::List;
        self.ui.project_panel.bin = if list || !found { None } else { bins.last().copied() };
        if list {
            for b in bins {
                if !self.ui.expanded_bins.contains(&b) {
                    self.ui.expanded_bins.push(b);
                }
            }
        }
        self.ui.project_panel.active_tab = None;
        self.ui.project_panel.selected_bin = None;
        self.ui.project_search.clear();
        self.show_panel(PanelKind::Project);
    }

    pub fn status(&mut self, s: impl Into<String>) {
        self.ui.status = s.into();
    }

    // ---------------------------------------------------------------- playback

    pub fn toggle_play(&mut self, speed: f64) {
        if self.playback.playing {
            self.stop();
        } else {
            self.play(speed);
        }
    }

    pub fn play(&mut self, speed: f64) {
        self.stop_source();
        if self.session.active_sequence().is_none() {
            return;
        }
        // nothing plays behind the Project Manager (it covers the monitors; the sound would play
        // from a screen the user can't see): opening it stops playback, and Play waits for it to close
        if self.ui.home.open {
            self.status(tl!("Close the Project Manager to play"));
            return;
        }
        // restart from the end → from the start (Settings ▸ Timeline ▸ "At playback end, return to
        // beginning when restarting playback")
        let dur = self.session.active_sequence().map(|q| q.duration()).unwrap_or_default();
        if speed > 0.0 && self.session.prefs.timeline.return_to_beginning && self.session.playhead() >= dur - self.session.sequence_rate().frame_duration() {
            self.session.set_playhead(Tick::ZERO);
        }
        // A loop restart keeps counting into the same meter.
        if !self.playback.playing || self.playback.speed != speed {
            self.playback.meter.start(speed);
        }
        self.playback.playing = true;
        self.playback.speed = speed;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.anchor_time = -1.0; // set when the preroll ends
        self.playback.preroll = Some(-1.0);
        self.playback.preroll_ready = false;
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.playback.audio_clock = false;
    }

    /// Start the clock (and audio) once the first frames are ready or the preroll timed out.
    fn end_preroll(&mut self, now: f64) {
        self.playback.preroll = None;
        self.playback.anchor_time = now;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.audio_seen = (0, now);
        self.start_audio();
        // Audio Track Mixer: an automation pass runs while playing forward in real time
        if (self.playback.speed - 1.0).abs() < 1e-9 && !self.session.mixrec.active() {
            let t = self.session.playhead();
            let _ = self.session.execute("mixer.recordStart", json!({"time": t.0}));
        }
        // Multi-Camera view: playing records live cuts (keys 1–9 / clicking angles)
        panels::multicam::on_play(self);
        // voice-over: the capture starts with the audio clock
        panels::voiceover::on_play(self);
    }

    /// Play the last Text to Speech preview (`Session::tts_preview`) on the audio output, stopping
    /// timeline playback first. Starting playback again takes the output back.
    pub fn play_tts_preview(&mut self) -> Result<f64, String> {
        let audio = self.session.tts_preview.clone().ok_or("nothing to preview")?;
        if self.playback.playing {
            self.stop();
        }
        let a = self.audio.as_mut().ok_or("no audio output (check Settings ▸ Audio Hardware)")?;
        let sr = a.sample_rate().max(1);
        a.stop();
        let step = f64::from(audio.sample_rate.max(1)) / f64::from(sr);
        let seconds = audio.seconds();
        let mut pos = 0.0f64;
        let fill = Box::new(move |buf: &mut [f32], ch: usize| {
            for frame in buf.chunks_mut(ch.max(1)) {
                let i = pos as usize;
                let f = (pos - i as f64) as f32;
                let x0 = audio.samples.get(i).copied().unwrap_or(0.0);
                let x1 = audio.samples.get(i + 1).copied().unwrap_or(0.0);
                frame.fill(x0 + (x1 - x0) * f);
                pos += step;
            }
        });
        a.start(fill)?;
        self.playback.audio_clock = false;
        Ok(seconds)
    }

    pub fn stop(&mut self) {
        self.playback.playing = false;
        self.playback.stop_at = None;
        self.playback.preroll = None;
        self.playback.meter.finish();
        self.frames.stop_prefetch();
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.playback.audio_clock = false;
        if self.session.mixrec.active() {
            let t = self.session.playhead();
            if let Err(e) = self.session.execute("mixer.recordStop", json!({"time": t.0})) {
                self.ui.status = e.to_string();
            }
        }
        panels::multicam::on_stop(self);
        panels::voiceover::on_stop(self);
    }

    fn start_audio(&mut self) {
        self.start_audio_at_rate(true);
    }

    /// `rate_retry`: a device that opened at another rate than it reported before (`configure`
    /// read it; the default sink changed in between, or the rate it was asked for isn't
    /// available) gets the mix rebuilt once at the rate it really plays (fork A1.3b): a mix built
    /// for the old rate would play at the wrong pitch and speed.
    fn start_audio_at_rate(&mut self, rate_retry: bool) {
        let speed = self.playback.speed;
        // normal speed, and shuttling (JKL) up to 2× either way, tape-style
        if !shuttle_audio::audible(speed) {
            if let Some(a) = self.audio.as_mut() {
                a.stop();
            }
            return;
        }
        let Some(seq_id) = self.session.state.active_sequence else { return };
        let start_tick = self.session.playhead();
        let Some(a) = self.audio.as_mut() else { return };
        let sr = a.sample_rate();
        let cursor = start_tick.to_units_floor(sr as i64);
        let mix = shuttle_audio::varispeed(playback_mix(&self.session, seq_id, sr), cursor, speed);
        // the old stream stops before the new mixer resets the underrun counters
        a.stop();
        // Desktop: mix ahead on a thread so the device callback never waits on decoding.
        #[cfg(not(target_arch = "wasm32"))]
        let fill = play_ahead::spawn(mix, cursor, sr, a.channels(), self.playback.audio_stats.clone());
        #[cfg(target_arch = "wasm32")]
        let fill = {
            let (mut mix, mut cursor) = (mix, cursor);
            Box::new(move |buf: &mut [f32], ch: usize| {
                mix(cursor, buf, ch);
                cursor += (buf.len() / ch.max(1)) as i64;
            })
        };
        match a.start(fill) {
            Ok(started) if started != sr && started > 0 && rate_retry => {
                log::warn!("the audio output opened at {started} Hz, not {sr} Hz; mixing again at {started} Hz");
                a.stop();
                self.start_audio_at_rate(false);
            }
            Ok(started) if started != sr => {
                // still another rate: sound at the wrong pitch and a clock running off the
                // picture would be worse than no sound
                log::warn!("the audio output keeps changing its rate ({sr} Hz, then {started} Hz); playing without sound");
                a.stop();
                self.playback.audio_clock = false;
                self.ui.status = tl!("No sound: the audio output keeps changing its sample rate (check Settings ▸ Audio Hardware)").into();
            }
            Ok(_) => {
                self.playback.audio_clock = true;
                if let Some(note) = self.audio.as_ref().and_then(|audio| audio.note()) {
                    self.ui.status = note;
                }
            }
            Err(e) => {
                log::warn!("audio output unavailable: {e}");
                self.playback.audio_clock = false;
                // fork A1.2: say so instead of playing silently without a word
                let why: String = e.chars().filter(|c| !c.is_control()).take(160).collect();
                self.ui.status = tlf!("No sound: the audio output could not start ({why})", why).to_string();
            }
        }
    }

    fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.playback.playing {
            return;
        }
        let now = ctx.input(|i| i.time);
        if let Some(since) = self.playback.preroll {
            let since = if since < 0.0 { now } else { since };
            self.playback.preroll = Some(since);
            if self.playback.preroll_ready || now - since >= frames::preroll_timeout(self.playback.speed) {
                self.end_preroll(now);
            } else {
                ctx.request_repaint();
                return;
            }
        }
        if self.playback.anchor_time < 0.0 {
            self.playback.anchor_time = now;
        }
        let rate = self.session.sequence_rate();
        if self.playback.audio_clock
            && let Some(audio) = self.audio.as_mut()
        {
            audio.recover();
        }
        let mut reading = if self.playback.audio_clock { self.audio.as_ref().and_then(|a| a.played_frames().map(|f| (f, a.sample_rate()))) } else { None };
        // Fork A1.2: the mixer stopped after repeated panics: the device only plays silence now,
        // so say so once and go on with the wall clock
        if self.playback.audio_clock && self.playback.mixer_stopped() {
            self.playback.anchor_tick = self.session.playhead();
            self.playback.anchor_time = now;
            self.playback.audio_clock = false;
            if let Some(audio) = self.audio.as_mut() {
                audio.stop();
            }
            self.ui.status = tl!("Audio mixing stopped after an error: playing without sound").into();
            reading = None;
        } else if self.playback.audio_clock && reading.is_none() {
            // A device error relinquishes its clock immediately. Resume from the displayed
            // frame, rather than jumping to the old wall-clock anchor after buffered playback.
            self.playback.anchor_tick = self.session.playhead();
            self.playback.anchor_time = now;
            self.playback.audio_clock = false;
            if let Some(audio) = self.audio.as_mut() {
                audio.stop();
            }
            log::warn!("audio output lost its playback clock; playing without sound");
            self.ui.status = tl!("Audio output failed: playing without sound (check Settings ▸ Audio Hardware)").into();
        }
        if let Some((f, sr)) = reading {
            if f != self.playback.audio_seen.0 {
                self.playback.audio_seen = (f, now);
            } else if now - self.playback.audio_seen.1 >= AUDIO_STALL_S {
                // the device stopped consuming samples: continue from where the audio got to on
                // the wall clock, without sound
                let played = if sr > 0 { f as f64 / sr as f64 } else { 0.0 };
                self.playback.anchor_tick += Tick::from_seconds_f64(played * self.playback.speed);
                self.playback.anchor_time = now;
                self.playback.audio_clock = false;
                if let Some(a) = self.audio.as_mut() {
                    a.stop();
                }
                log::warn!("audio output stalled (no samples consumed for {AUDIO_STALL_S} s); playing without sound");
                self.ui.status = tl!("Audio output is not responding: playing without sound (check Settings ▸ Audio Hardware)").into();
            }
        }
        let elapsed = match reading {
            Some((f, sr)) if self.playback.audio_clock && sr > 0 => f as f64 / sr as f64,
            _ => now - self.playback.anchor_time,
        };
        let t = self.playback.anchor_tick + Tick::from_seconds_f64(elapsed * self.playback.speed);
        let seq = self.session.active_sequence();
        let dur = seq.map(|q| q.duration()).unwrap_or_default();
        let (lo, hi) = if self.playback.looping {
            (seq.and_then(|q| q.mark_in).unwrap_or(Tick::ZERO), seq.and_then(|q| q.mark_out).map(|o| o + rate.frame_duration()).unwrap_or(dur))
        } else {
            (Tick::ZERO, dur)
        };
        if let Some(end) = self.playback.stop_at.filter(|e| t >= *e && self.playback.speed > 0.0 && !self.playback.looping) {
            self.session.set_playhead(end);
            self.stop();
        } else if t >= hi && self.playback.speed > 0.0 {
            if self.playback.looping {
                self.session.set_playhead(lo);
                self.play(self.playback.speed);
            } else {
                self.session.set_playhead(hi - rate.frame_duration());
                self.stop();
            }
        } else if t <= Tick::ZERO && self.playback.speed < 0.0 {
            self.session.set_playhead(Tick::ZERO);
            self.stop();
        } else {
            self.session.set_playhead(t);
        }
        ctx.request_repaint();
    }

    // ---------------------------------------------------------------- textures

    /// Upload a rendered frame into a named texture (only when the key changed).
    pub fn texture_for(&mut self, ctx: &egui::Context, name: &str, key: FrameKey, img: &frames::Rgba) -> egui::TextureId {
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                tex.set(egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.px), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let tex = ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.px), TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(name.to_string(), (key, tex));
        id
    }

    /// Like [`Self::texture_for`], uploading `map(img)` (computed only when the key changed).
    pub fn texture_for_mapped(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        key: FrameKey,
        img: &frames::Rgba,
        map: impl FnOnce(&frames::Rgba) -> frames::Rgba,
    ) -> egui::TextureId {
        if let Some((k, tex)) = self.textures.get_mut(name) {
            if *k != key {
                let m = map(img);
                tex.set(egui::ColorImage::from_rgba_unmultiplied([m.w, m.h], &m.px), TextureOptions::LINEAR);
                *k = key;
            }
            return tex.id();
        }
        let m = map(img);
        let tex = ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied([m.w, m.h], &m.px), TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(name.to_string(), (key, tex));
        id
    }

    pub fn texture_existing(&self, name: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        self.textures.get(name).map(|(_, t)| (t.id(), t.size_vec2()))
    }

    /// The frame a named texture shows.
    pub fn texture_key(&self, name: &str) -> Option<FrameKey> {
        self.textures.get(name).map(|(k, _)| *k)
    }

    /// The frame whose picture the Program monitor drew last: the frame due at the playhead, or
    /// the stand-in shown while that one renders (`perf.stats`, tests).
    pub fn program_picture(&self) -> Option<FrameKey> {
        self.program_shown
    }

    /// Get a thumbnail texture for an item at a media time (requested at low priority).
    /// Cache revision of a project item's own frames: media changes only when its file does
    /// (relink, Make Offline, proxies on/off), so its frames survive unrelated edits; other items
    /// (sequences) follow the project revision.
    pub fn item_revision(&self, item: filmcraft_project::ItemId) -> u64 {
        let p = &self.session.project;
        let target = match p.item(item).map(|i| &i.kind) {
            Some(filmcraft_project::ItemKind::Subclip { parent, .. }) => *parent,
            _ => item,
        };
        match p.item(target).map(|i| &i.kind) {
            Some(filmcraft_project::ItemKind::Media(m)) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                filmcraft_engine::media_pool::media_key(m).hash(&mut h);
                (m.proxy.is_some() && self.session.media.use_proxies()).hash(&mut h);
                h.finish()
            }
            _ => self.session.revision,
        }
    }

    pub fn thumbnail(&mut self, ctx: &egui::Context, item: filmcraft_project::ItemId, t: Tick, width: u32) -> Option<(egui::TextureId, egui::Vec2)> {
        let pi = self.session.project.item(item)?;
        let src_w = match &pi.kind {
            filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref()?.width,
            filmcraft_project::ItemKind::Sequence(s) => s.settings.width,
            _ => return None,
        };
        let rate = pi.frame_rate();
        let frame = rate.frame_at(t);
        let rev = self.item_revision(item);
        let key = FrameKey { target: Target::Item(item), frame, size: width, revision: rev, draft: false, temporal: false };
        let name = format!("thumb-{}-{}-{}", item.0, frame, width);
        if let Some(img) = self.frames.get(&key) {
            let id = self.texture_for(ctx, &name, key, &img);
            return Some((id, egui::vec2(img.w as f32, img.h as f32)));
        }
        let scale = width as f32 / src_w.max(1) as f32;
        let project = self.session.project.clone();
        self.frames.request(key, rate.tick_of(frame), scale, &project, 50);
        self.texture_existing(&name)
    }

    // ---------------------------------------------------------------- files

    /// Destination of imports started from the currently shown Project panel view.
    pub fn import_bin(&self) -> filmcraft_project::BinId {
        panels::project::view_of(self, panels::project::shown_inst(self)).bin
    }

    pub fn file_dialog(&mut self, id: &str, params: &Value) -> Result<Value, String> {
        match id {
            "file.import" => {
                let exts: Vec<&str> = filmcraft_media::VIDEO_EXTENSIONS
                    .iter()
                    .chain(filmcraft_media::AUDIO_EXTENSIONS)
                    .chain(filmcraft_media::STILL_EXTENSIONS)
                    .chain(&["srt", "vtt", "scc", "edl", "xml", "fcpxml", "otio", "aaf", "omf"])
                    .copied()
                    .collect();
                let bin = params.get("bin").cloned();
                self.pick(FileDialog::open_files(&exts), move |app, paths| {
                    if paths.is_empty() {
                        return Ok(Value::Null);
                    }
                    let bin = bin.unwrap_or_else(|| json!(app.import_bin().0));
                    let r = app.session.execute("file.import", json!({"paths": paths, "bin": bin})).map_err(|e| e.to_string());
                    if let Ok(v) = &r
                        && let Some(errs) = v.get("errors").and_then(Value::as_array)
                        && !errs.is_empty()
                    {
                        app.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
                    }
                    r
                })
            }
            // File ▸ Import with Image Sequence: choose the first numbered still
            "file.importImageSequence" => self.pick(FileDialog::open_files(filmcraft_media::STILL_EXTENSIONS), |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                let r = app.session.execute("file.importImageSequence", json!({"path": path})).map_err(|e| e.to_string());
                if let Err(e) = &r {
                    app.ui.status = e.clone();
                }
                r
            }),
            "file.saveAs" | "file.save" | "file.saveCopy" => {
                let suggested =
                    if id == "file.saveCopy" { format!("{} copy.fcproj", self.session.project.name) } else { format!("{}.fcproj", self.session.project.name) };
                let cmd = if id == "file.saveCopy" { "file.saveCopy" } else { "file.saveAs" };
                self.pick(FileDialog::SaveProject { name: suggested }, move |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    app.session.execute(cmd, json!({"path": path})).map_err(|e| e.to_string())
                })
            }
            "file.open" => self.pick(FileDialog::OpenProject, |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                app.session.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())
            }),
            "graphics.newFromFile" => {
                let exts: Vec<&str> = filmcraft_media::STILL_EXTENSIONS.iter().chain(filmcraft_media::VIDEO_EXTENSIONS).copied().collect();
                self.pick(FileDialog::open_files(&exts), |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    app.session.execute("graphics.newFromFile", json!({"path": path})).map_err(|e| e.to_string())
                })
            }
            "stills.import" => self.pick(FileDialog::open_files(filmcraft_media::STILL_EXTENSIONS), |app, paths| {
                // every picture chosen; the first error answers (the others are imported anyway)
                let mut out = Ok(Value::Null);
                for path in paths {
                    match app.session.execute("stills.import", json!({"path": path})) {
                        Ok(v) if out.is_ok() => out = Ok(v),
                        Ok(_) => {}
                        Err(e) if out.is_ok() => out = Err(e.to_string()),
                        Err(e) => app.session.toast(e.to_string()),
                    }
                }
                out
            }),
            "captions.import" => self.pick(FileDialog::open_files(&["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp", "xml"]), |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                app.session.execute("captions.import", json!({"path": path})).map_err(|e| e.to_string())
            }),
            "captions.export" => {
                let name = self.session.state.active_sequence.and_then(|s| self.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default();
                let suggested = format!("{}.srt", name.replace(' ', "_"));
                let dialog = FileDialog::save_as(
                    "Captions (SRT, WebVTT, SCC, MCC, EBU STL, TTML, DFXP)",
                    &["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp"],
                    &suggested,
                );
                let mut p = params.clone();
                self.pick(dialog, move |app, paths| {
                    let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                    p["path"] = json!(path);
                    app.session.execute("captions.export", p).map_err(|e| e.to_string())
                })
            }
            _ => match EXPORT_SAVE_DIALOGS.iter().find(|(c, ..)| *c == id) {
                Some(&(_, filter, ext)) => self.export_save_dialog(id, filter, ext, params),
                None => Err(format!("no dialog for {id}")),
            },
        }
    }

    /// File ▸ Export ▸ EDL…, Final Cut Pro XML… etc. from the menus (#382): ask where to save, then
    /// run the command with that `path` and the rest of `params`.
    fn export_save_dialog(&mut self, id: &str, filter: &str, ext: &str, params: &Value) -> Result<Value, String> {
        filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&self.session))?;
        if !FileDialog::save_as(filter, &[ext], "").available(&self.hooks) {
            return Err("no save dialog available: run the command with a `path`".into());
        }
        // Timelines are named after the sequence; a selection must not suggest the open project's file.
        let sequence = self.session.state.active_sequence.and_then(|s| self.session.project.item(s)).map(|i| i.name.clone());
        let stem = match id {
            "file.exportAle" => self.session.project.name.clone(),
            "file.exportSelectionProject" => format!("{} selection", self.session.project.name),
            _ => sequence.unwrap_or_else(|| self.session.project.name.clone()),
        };
        let stem: String = stem.chars().map(|c| if matches!(c, '/' | '\\') || c.is_control() { '-' } else { c }).collect();
        let stem = if stem.trim().is_empty() { "Untitled".to_string() } else { stem };
        let mut p = params.as_object().cloned().unwrap_or_default();
        let id = id.to_string();
        self.pick(FileDialog::save_as(filter, &[ext], &format!("{stem}.{ext}")), move |app, paths| {
            let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
            p.insert("path".into(), json!(path));
            let r = app.session.execute(&id, Value::Object(p)).map_err(|e| e.to_string());
            app.ui.status = match &r {
                Ok(_) => tlf!("Exported {path}", path),
                Err(e) => e.clone(),
            };
            r
        })
    }

    /// Import dropped files. With Timeline ▸ drop imports to the timeline, the new items are also
    /// queued to be placed at the pointer (see `timeline::interact`).
    fn handle_drops(&mut self, ctx: &egui::Context) {
        // While files hover, the OS sends no cursor events: poll the cursor each frame to draw the Timeline preview.
        let hovering = ctx.input(|i| i.raw.hovered_files.len());
        self.file_drag_hover = if hovering > 0 && self.session.prefs.timeline.drop_import_to_timeline {
            ctx.request_repaint();
            self.drop_pointer(ctx).map(|p| (hovering, p))
        } else {
            None
        };
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let mut paths = Vec::new();
        for f in dropped {
            let p = f.path();
            if p.exists() {
                paths.push(p.to_string_lossy().to_string());
            }
        }
        // pictures dropped on the Gallery strip become reference stills (REF.1)
        if !paths.is_empty()
            && let Some((strip, pass)) = ctx.data(|d| d.get_temp::<(egui::Rect, u64)>(egui::Id::new(crate::panels::stills::STRIP_RECT)))
            && ctx.cumulative_pass_nr().saturating_sub(pass) <= 1
            && self.drop_pointer(ctx).is_some_and(|p| strip.contains(p))
        {
            // (only pictures: anything else dropped there is imported as media, as anywhere else)
            let is_picture = |p: &String| {
                let ext = std::path::Path::new(p).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                filmcraft_media::STILL_EXTENSIONS.contains(&ext.as_str())
            };
            let (pictures, rest): (Vec<String>, Vec<String>) = std::mem::take(&mut paths).into_iter().partition(is_picture);
            paths = rest;
            for p in pictures {
                if let Err(e) = self.session.execute("stills.import", json!({"path": p})) {
                    self.session.toast(e.to_string());
                }
            }
        }
        if !paths.is_empty() {
            let pointer = self.drop_pointer(ctx);
            let imported = self.session.execute("file.import", json!({"paths": paths, "bin": self.import_bin().0}));
            if let Ok(v) = imported
                && let Some(pos) = pointer
                && self.session.prefs.timeline.drop_import_to_timeline
            {
                let items: Vec<filmcraft_engine::project::ItemId> =
                    v["items"].as_array().map(|a| a.iter().filter_map(|i| i.as_u64()).map(filmcraft_engine::project::ItemId).collect()).unwrap_or_default();
                if !items.is_empty() {
                    self.pending_timeline_drop = Some((items, pos));
                }
            }
        }
    }

    /// Where files dragged in from the OS are over the window. The OS reports no cursor motion during such a drag
    /// (winit, Windows), so ask it through the host hook, else use the last pointer position egui saw.
    fn drop_pointer(&self, ctx: &egui::Context) -> Option<egui::Pos2> {
        let os_pointer = self
            .hooks
            .cursor_screen_position
            .as_ref()
            .and_then(|f| f())
            .and_then(|px| ctx.input(|i| i.viewport().inner_rect.map(|r| screen_px_to_ui(px, i.pixels_per_point, r.min))));
        os_pointer.or_else(|| ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())))
    }

    // ---------------------------------------------------------------- input

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let workspaces = (dock::names_for(&self.workspaces, self.resolve_workflow()), self.ui.workspace.clone());
        if self.bindings_rev != self.session.shortcuts.revision || workspaces != self.menu_workspaces {
            self.menu_workspaces = workspaces;
            self.bindings = menus::bindings(self);
            self.bindings_rev = self.session.shortcuts.revision;
            let items = menus::menu_items(self);
            if let Some(hook) = self.hooks.shortcuts_changed.as_mut() {
                hook(&items);
            }
        }
        if ctx.egui_wants_keyboard_input() || self.dialog == Some(Dialog::Shortcuts) {
            return;
        }
        // Esc cancels a dynamic trim in progress
        if self.session.trim_play.dynamic.is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            let _ = self.session.execute("trim.cancelDynamic", json!({}));
        }
        // Panel shortcuts of the focused panel override application shortcuts on the same chord.
        // egui ignores extra Shift and Alt when matching, so the most specific chords go first
        // across both scopes (the sort is stable, so the panel still wins a tie): otherwise the
        // Timeline's `Delete` (Clear) swallows the app-wide `Shift+Delete` (Ripple Delete) (#680).
        let focused = self.ui.focused.title();
        let mut fire = Vec::new();
        ctx.input_mut(|i| {
            let modifiers = i.modifiers;
            // With no text on the system clipboard, egui-winit drops the Ctrl+V press but still
            // passes its release: put the press back, so Paste can paste files or an image (#611).
            let mut swallowed = None;
            for e in &i.events {
                match e {
                    egui::Event::Key { key: egui::Key::V, pressed, modifiers: m, .. } => {
                        if !pressed && !self.v_down {
                            swallowed = Some(*m);
                        }
                        self.v_down = *pressed;
                    }
                    egui::Event::Paste(_) if modifiers.command => self.v_down = true,
                    _ => {}
                }
            }
            if let Some(m) = swallowed {
                i.events.push(egui::Event::Key {
                    key: egui::Key::V,
                    physical_key: Some(egui::Key::V),
                    pressed: true,
                    repeat: false,
                    modifiers: m | egui::Modifiers::COMMAND,
                });
            }
            clipboard_events_as_keys(&mut i.events, modifiers);
            let panel = self.bindings.iter().filter(|b| b.3.as_deref() == Some(focused));
            let app_wide = self.bindings.iter().filter(|b| b.3.is_none());
            let mut candidates: Vec<&menus::KeyBinding> = panel.chain(app_wide).collect();
            candidates.sort_by_key(|b| std::cmp::Reverse(menus::specificity(b.0)));
            for (m, k, id, _) in candidates {
                if i.consume_key(*m, *k) {
                    fire.push(id.clone());
                }
            }
        });
        for mut id in fire {
            // Select All / Deselect All act on the Project panel's items when it has focus (#168).
            // Select All takes only what the shown bin lists, not the whole project (#456).
            if self.ui.focused == PanelKind::Project && id == "edit.selectAll" {
                let (cmd, params) = panels::project::select_all_command(self);
                if let Err(e) = menus::invoke(self, ctx, &cmd, params) {
                    self.ui.status = e;
                }
                continue;
            }
            if self.ui.focused == PanelKind::Project && id == "edit.deselectAll" {
                id = "project.deselectAll".into();
            }
            // Clear in the Project panel removes the selected bin when no item is selected (#587).
            let clear = if self.ui.focused == PanelKind::Project && id == "project.delete" {
                self.ui.project_panel.rename = None;
                crate::panels::project_views::clear_params(&self.session.state.project_selection, self.ui.project_panel.selected_bin)
            } else {
                None
            };
            // Mark In/Out in the Source monitor when it has focus.
            let params = if let Some(p) = clear {
                p
            } else if self.ui.focused == PanelKind::Source
                && (matches!(id.as_str(), "markers.markIn" | "markers.markOut" | "markers.clearInOut")
                    || id.starts_with("markers.markSplit")
                    || id.starts_with("markers.goToSplit"))
            {
                json!({"target": "source"})
            } else {
                json!({})
            };
            if let Err(e) = menus::invoke(self, ctx, &id, params) {
                self.ui.status = e;
            }
        }
    }

    // ---------------------------------------------------------------- control channel

    /// Make the UI pass run for a control request without stealing the user's keyboard focus.
    pub(crate) fn raise_for_control(&mut self, ctx: &egui::Context) {
        if let Some(raise) = self.hooks.raise_without_focus.as_mut() {
            raise();
        }
        ctx.request_repaint();
    }

    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        let now = ctx.input(|i| i.time);
        let mut reqs: Vec<(ControlRequest, f64)> = std::mem::take(&mut self.deferred);
        while let Ok(req) = rx.try_recv() {
            // UI requests need rendered frames: raise the window if `ui` hasn't run recently
            // (occluded macOS windows stop running `ui`).
            if req.method.starts_with("ui.") && now - self.last_ui_time > 0.25 {
                self.raise_for_control(ctx);
            }
            reqs.push((req, now + 3.0));
        }
        for (req, deadline) in reqs {
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Retry(msg) => {
                    if now < deadline {
                        self.deferred.push((req, deadline));
                        ctx.request_repaint();
                    } else {
                        let _ = reply.send(json!({"ok": false, "error": msg}));
                    }
                }
                control::Outcome::AfterInput => self.input_waiters.push(reply),
                control::Outcome::Screenshot { path, crop } => {
                    let token = self.next_token;
                    self.next_token += 1;
                    let settle = ctx.input(|i| i.time) + 0.25;
                    self.queued_screenshots.push((token, settle, 0));
                    self.pending_screenshots.push((token, path, crop, reply, settle + SCREENSHOT_TIMEOUT_S));
                }
            }
        }
        self.control_rx = Some(rx);
    }

    /// Capture once the UI shows the current state (`settled`): after a seek the monitor shows the
    /// nearest cached picture until the exact frame is decoded, and an agent must not be handed
    /// the stand-in.
    fn issue_screenshots(&mut self, ctx: &egui::Context, settled: bool) {
        let now = ctx.input(|i| i.time);
        let mut any = false;
        self.queued_screenshots.retain_mut(|(token, at, frames)| {
            *frames += 1;
            any = true;
            if now >= *at && *frames >= 3 && (settled || now >= *at + SCREENSHOT_SETTLE_MAX_S) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(*token)));
                false
            } else {
                true
            }
        });
        // A hidden window (or a sleeping display) never presents, so its screenshot never
        // arrives: give up instead of waiting (and repainting) forever.
        self.pending_screenshots.retain(|(.., reply, deadline)| {
            if now > *deadline {
                let _ = reply.send(json!({"ok": false, "error": "no frame was presented (window hidden or display asleep)"}));
                false
            } else {
                true
            }
        });
        if any {
            ctx.request_repaint();
        } else if !self.pending_screenshots.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    fn collect_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let events: Vec<_> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let token = user_data.data.as_ref().and_then(|d| d.downcast_ref::<u64>()).copied()?;
                        Some((token, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (token, image) in events {
            if let Some(i) = self.pending_screenshots.iter().position(|(t, ..)| *t == token) {
                let (_, path, crop, reply, _) = self.pending_screenshots.remove(i);
                let r = control::save_screenshot(ctx, &image, path.as_deref(), crop);
                let _ = reply.send(r);
            }
        }
    }

    // ---------------------------------------------------------------- frame

    /// Local copies of share clips (STORAGE-PLAN S1 item 2): one file at a time while not
    /// playing; a copy that finishes switches its clip over and says so.
    fn tick_net_copies(&mut self, ctx: &egui::Context) {
        if let Some((source, r)) = self.session.poll_net_copy().filter(|(_, r)| r.as_ref().err().is_none_or(|e| e != "stopped")) {
            let name = std::path::Path::new(&source).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let text = match r {
                Ok(_) => tlf!("Local copy of {name} ready: it now plays from this computer", name = name),
                Err(e) => tlf!("Couldn't copy {name} from the network share: {error}", name = name, error = e),
            };
            self.toast = Some((text, ctx.input(|i| i.time)));
        }
        if self.playback.playing {
            if let Some(job) = &self.session.net_copy {
                job.stop();
            }
            return;
        }
        let key = egui::Id::new("netCopies.lastTry");
        let now = ctx.input(|i| i.time);
        let last: f64 = ctx.data(|d| d.get_temp(key)).unwrap_or(f64::NEG_INFINITY);
        if self.session.net_copy.is_none() && now - last >= 5.0 {
            ctx.data_mut(|d| d.insert_temp(key, now));
            self.session.start_net_copy();
        }
        if self.session.net_copy.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    /// Settings ▸ Media ▸ Automatically refresh growing files: every *Refresh growing files every*
    /// seconds, and as soon as the window comes back to the front (after a render in another app),
    /// stamp the project's media files on a worker thread and read the changed ones again
    /// ([`filmcraft_engine::media_watch`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_media_refresh(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.session.poll_media_scan() {
            match r {
                Ok(r) => {
                    if let Some(n) = r["refreshed"].as_array().map(Vec::len).filter(|n| *n > 0) {
                        self.toast = Some((format!("Refreshed {n} media item(s) changed on disk"), ctx.input(|i| i.time)));
                    }
                    // a share that dropped or came back (STORAGE-PLAN S1 item 3b)
                    if let Some(n) = r["offline"].as_array().map(Vec::len).filter(|n| *n > 0) {
                        self.toast = Some((tlf!("{n} clip(s) went offline: their files can't be reached", n = n), ctx.input(|i| i.time)));
                    }
                    if let Some(n) = r["online"].as_array().map(Vec::len).filter(|n| *n > 0) {
                        self.toast = Some((tlf!("{n} clip(s) are back online", n = n), ctx.input(|i| i.time)));
                    }
                }
                Err(e) => self.toast = Some((e.to_string(), ctx.input(|i| i.time))),
            }
        }
        let media = &self.session.prefs.media;
        let (on, every) = (media.refresh_growing_files, f64::from(media.growing_refresh_seconds.max(1)));
        let (last_key, focus_key) = (egui::Id::new("mediaWatch.lastScan"), egui::Id::new("mediaWatch.focused"));
        let now = ctx.input(|i| i.time);
        let focused = ctx.input(|i| i.focused);
        let was_focused: bool = ctx.data(|d| d.get_temp(focus_key)).unwrap_or(focused);
        ctx.data_mut(|d| d.insert_temp(focus_key, focused));
        if !on || filmcraft_engine::media_watch::media_files(&self.session.project).is_empty() {
            return;
        }
        let last: f64 = ctx.data(|d| d.get_temp(last_key)).unwrap_or(f64::NEG_INFINITY);
        if now - last >= every || (focused && !was_focused) {
            ctx.data_mut(|d| d.insert_temp(last_key, now));
            self.session.start_media_scan();
        }
        let wait = if self.session.media_scan.is_some() { 0.1 } else { (every - (now - last)).clamp(0.1, every) };
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
    }

    fn frame(&mut self, ui: &mut egui::Ui) {
        i18n::set_current(self.ui.language);
        i18n::terms::set_resolve(self.resolve_workflow());
        if std::mem::take(&mut self.panic_next_frame) {
            crash::injected_fault("injected UI fault");
        }
        let ctx = ui.ctx().clone();
        self.auto.begin_frame();
        self.frames.set_context(&ctx);
        self.session.poll_persistence();
        panels::trim_monitor::advance(self, &ctx);
        if self.session.persistence.is_some() && self.session.is_dirty() {
            // Keep polling the auto-save worker (status, "also save the project" results).
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        self.sync_pool();
        self.apply_prefs(&ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_media_refresh(&ctx);
        self.tick_net_copies(&ctx);
        panels::update_prompt::tick(self, &ctx);
        for ev in self.session.drain_events() {
            match ev {
                filmcraft_engine::Event::OpenSequence(_) => {
                    // show the sequence with its own view (or fitted, the first time)
                    self.timeline_view_of = None;
                    if !self.ui.floating.iter().any(|group| group.panels.contains(&PanelKind::Timeline)) {
                        self.ui.dock.restore_timeline();
                    }
                    let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: PanelKind::Timeline });
                }
                filmcraft_engine::Event::OpenSource(_) => {
                    let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: PanelKind::Source });
                }
                filmcraft_engine::Event::RevealInProject(item) => self.reveal_in_project(item),
                filmcraft_engine::Event::Toast { message, .. } => self.toast = Some((message, ctx.input(|i| i.time))),
                filmcraft_engine::Event::ProjectChanged { .. } => {}
            }
        }
        self.sync_timeline_view();
        // fork RS2: posters of projects just saved
        panels::home::tick_posters(self, &ctx);
        let home = self.ui.home.open;
        if !home {
            self.handle_drops(&ctx);
        }
        if let Some(rx) = self.command_inbox.take() {
            while let Ok(id) = rx.try_recv() {
                if self.control_input_only {
                    continue;
                }
                if let Err(e) = menus::invoke(self, &ctx, &id, json!({})) {
                    self.ui.status = e;
                }
            }
            self.command_inbox = Some(rx);
        }
        if !home {
            self.handle_shortcuts(&ctx);
        }
        // after the commands of this frame, so a click in the global menu shows its effect there
        self.publish_menu(&ctx);
        self.advance_playback(&ctx);
        self.advance_source_playback(&ctx);
        self.scrub_audio(&ctx);
        // fork 6.2 / 6.3: the background media worker (proxies, render cache) waits while the
        // user plays, scrubs (a playhead move while stopped) or exports
        self.session.background.observe(
            self.playback.playing || self.source_playback.clock.playing,
            filmcraft_engine::background::foreground_jobs_running(&self.session),
            Some(self.session.playhead()),
        );
        // Text to Speech: a selected narration clip loads into the panel
        panels::tts::follow_selection(self);
        if self.ui.tts.pending.is_some() || self.ui.tts.download_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // fork RS3: the Resolve workflow's pages (Export mode is the Deliver page there)
        panels::pages::follow_workflow(self);
        let t = self.tokens;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, t.app_bg);
        if home {
            // the Project Manager covers the window (no panels behind it); dialogs such as
            // crash recovery still show over it
            panels::home::draw(self, ui, full);
            panels::dialogs::show(self, &ctx);
            self.watch_jobs(ui.ctx());
            return;
        }
        // the Resolve workflow has its own header: the menus, then the page's toolbar (fork RS3)
        let resolve = self.resolve_workflow();
        let header_h = match (self.ui.show_header, resolve) {
            (false, _) => 0.0,
            (true, false) => 38.0,
            (true, true) => panels::page_toolbar::height(self),
        };
        let header = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), header_h));
        if self.ui.show_header {
            if resolve {
                panels::page_toolbar::show(self, ui, header);
            } else {
                header::show(self, ui, header);
            }
        }
        let status_h = if self.ui.show_status_bar { 20.0 } else { 0.0 };
        // Resolve workflow: the page bar sits above the status bar (GRADE-PLAN G1.8)
        let pages_h = if resolve { panels::pages::HEIGHT } else { 0.0 };
        let body =
            egui::Rect::from_min_max(egui::pos2(full.min.x + 1.0, header.max.y + 1.0), egui::pos2(full.max.x - 1.0, full.max.y - status_h - pages_h - 2.0));
        match self.ui.mode {
            state::Mode::Edit => self.dock_area(ui, body),
            state::Mode::Import => panels::import_mode::show(self, ui, body),
            state::Mode::Export => panels::export_mode::show(self, ui, body),
        }
        if pages_h > 0.0 {
            let bar = egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - status_h - pages_h), egui::pos2(full.max.x, full.max.y - status_h));
            panels::pages::show(self, ui, bar);
        }
        // the Timeline has had its chance to place a file drop this frame; anything left stays a bin import
        self.pending_timeline_drop = None;
        panels::dialogs::show(self, &ctx);
        // Grading Coach: the lesson card and the spotlight on the control it is about
        panels::coach::show(self, &ctx);
        if !self.ui.show_status_bar {
            // no bar to draw the job in, but a finished preview render still plays
            self.watch_jobs(ui.ctx());
            return;
        }
        // Status / hint bar
        let sb = egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - status_h), full.max);
        ui.painter().rect_filled(sb, 0.0, t.header_bg);
        let now = ui.input(|i| i.time);
        if self.ui.status != self.status_seen.0 {
            self.status_seen = (self.ui.status.clone(), now);
        } else if !self.ui.status.is_empty() {
            if now - self.status_seen.1 > 8.0 {
                self.ui.status.clear();
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
        }
        let hint = if !self.ui.status.is_empty() { self.ui.status.clone() } else { self.hint_text() };
        ui.painter().text(egui::pos2(sb.min.x + 10.0, sb.center().y), egui::Align2::LEFT_CENTER, hint, Tokens::ui(11.0), t.text_dim);
        let resp = ui.interact(sb, egui::Id::new("status-bar"), egui::Sense::click());
        if resp.clicked() {
            self.ui.status.clear();
        }
        self.job_status(ui, sb, &t);
    }

    /// The running job (export / render previews), if any; plays the rendered range when a preview
    /// render completes. Runs every frame, with or without the status bar.
    fn watch_jobs(&mut self, ctx: &egui::Context) -> Option<filmcraft_engine::Job> {
        use std::sync::atomic::Ordering;
        let running = self.session.jobs.iter().rev().find(|j| !j.progress.finished.load(Ordering::Relaxed)).cloned();
        // Play after rendering previews.
        if let Some((id, from)) = self.watched_render
            && let Some(j) = self.session.jobs.iter().find(|j| j.id == id)
            && j.progress.finished.load(Ordering::Relaxed)
        {
            self.watched_render = None;
            let ok = j.progress.error.lock().map(|e| e.is_none()).unwrap_or(false);
            if ok && self.ui.play_after_render && !self.playback.playing {
                self.session.set_playhead(from);
                self.play(1.0);
            }
        }
        let job = running?;
        if job.label.starts_with("Rendering ") && !job.label.contains("audio") && self.watched_render.is_none_or(|w| w.0 != job.id) {
            let from = self.session.active_sequence().and_then(|q| q.mark_in).unwrap_or(Tick::ZERO);
            self.watched_render = Some((job.id, from));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
        Some(job)
    }

    /// Right side of the status bar: the running job (export / render previews) with a progress
    /// bar and a cancel button.
    fn job_status(&mut self, ui: &mut egui::Ui, sb: egui::Rect, t: &Tokens) {
        use std::sync::atomic::Ordering;
        let Some(job) = self.watch_jobs(ui.ctx()) else { return };
        let f = job.progress.fraction().clamp(0.0, 1.0);
        let left = job.progress.eta().map(panels::left_text).unwrap_or_default();
        let cancel = egui::Rect::from_center_size(egui::pos2(sb.max.x - 14.0, sb.center().y), egui::vec2(14.0, 14.0));
        let bar = egui::Rect::from_min_size(egui::pos2(cancel.min.x - 128.0, sb.center().y - 3.0), egui::vec2(120.0, 6.0));
        let p = ui.painter();
        p.rect_filled(bar, 3.0, t.separator);
        p.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * f, bar.height())), 3.0, t.accent);
        let verb = if job.label.starts_with("Rendering") { crate::i18n::t(&job.label).to_string() } else { tl!("Exporting").to_string() };
        p.text(
            egui::pos2(bar.min.x - 8.0, sb.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{verb}… {:.0}%{left}", f * 100.0),
            Tokens::ui(11.0),
            t.text_dim,
        );
        let resp = ui.interact(cancel, egui::Id::new(("job-cancel", job.id)), egui::Sense::click());
        let c = if resp.hovered() { t.hot_text } else { t.text_dim };
        let k = 3.5;
        p.line_segment([cancel.center() - egui::vec2(k, k), cancel.center() + egui::vec2(k, k)], egui::Stroke::new(1.4, c));
        p.line_segment([cancel.center() + egui::vec2(-k, k), cancel.center() + egui::vec2(k, -k)], egui::Stroke::new(1.4, c));
        self.auto.add("status.job.cancel", cancel, &format!("Cancel {}", job.label));
        self.auto.add("status.job.progress", bar, &format!("{:.0}%{left}", f * 100.0));
        if resp.on_hover_text(tl!("Cancel")).clicked() {
            job.progress.cancel.store(true, Ordering::Relaxed);
            self.watched_render = None;
        }
    }

    /// Contextual hint for the status bar (Premiere shows tool/gesture hints here).
    fn hint_text(&self) -> String {
        let mac = cfg!(target_os = "macos");
        match self.ui.tool {
            state::Tool::Selection if mac => {
                tl!("Click to select, or click in empty space and drag to marquee select. Use Shift, Opt, and Cmd for other options.")
            }
            state::Tool::Selection => tl!("Click to select, or click in empty space and drag to marquee select. Use Shift, Alt, and Ctrl for other options."),
            state::Tool::TrackSelectForward => tl!("Click to select all clips to the right in all tracks. Shift-click for a single track."),
            state::Tool::TrackSelectBackward => tl!("Click to select all clips to the left in all tracks. Shift-click for a single track."),
            state::Tool::Ripple => tl!("Drag an edit point to ripple trim; later clips move to keep the gap closed."),
            state::Tool::Rolling => tl!("Drag an edit point to roll it: the out of one clip and the in of the next move together."),
            state::Tool::RateStretch => tl!("Drag an edge to change the clip's speed so it fills the new duration."),
            state::Tool::Remix => tl!("Drag the edge of a music clip to remix it to the new duration at musically matching beats."),
            state::Tool::Razor => tl!("Click to split a clip. Shift-click to split all tracks."),
            state::Tool::Slip => tl!("Drag a clip to slip its source in/out without moving it."),
            state::Tool::Slide => tl!("Drag a clip to slide it between its neighbours."),
            state::Tool::Hand => tl!("Drag to scroll the timeline."),
            state::Tool::Zoom if mac => tl!("Click to zoom in; Opt-click to zoom out."),
            state::Tool::Zoom => tl!("Click to zoom in; Alt-click to zoom out."),
            _ => "",
        }
        .to_string()
    }
}

impl FilmcraftApp {
    /// The "FilmCraft hit an error" window after a caught UI panic. Automation ids:
    /// `error.dismiss`, `error.save`, `error.report` (shows the crash log in the file manager and
    /// opens the issue tracker; nothing is uploaded).
    fn error_window(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.ui_error.clone() else { return };
        let mut close = false;
        egui::Window::new(tl!("Revolution hit an error"))
            .id(egui::Id::new("Revolution hit an error"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(tl!("Something went wrong while drawing the window. Your project is still open; save it to be safe."));
                ui.add_space(6.0);
                ui.label(egui::RichText::new(&msg).monospace().small());
                if let Some(p) = crash::log_path() {
                    ui.label(egui::RichText::new(tlf!("Details: {path}", path = p.display())).small());
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let s = ui.button(tl!("Save Project"));
                    self.auto.add("error.save", s.rect, "Save Project");
                    if s.clicked() {
                        let _ = crate::menus::invoke(self, ctx, "file.save", serde_json::json!({}));
                    }
                    let r = ui.button(tl!("Report an Issue…"));
                    self.auto.add("error.report", r.rect, "Report an Issue…");
                    if r.clicked() {
                        if let (Some(p), Some(open)) = (crash::log_path(), self.hooks.open_path.as_mut()) {
                            // past UTC midnight today's file may not exist yet: show the folder
                            let p = if p.exists() { p } else { p.parent().map(|d| d.to_path_buf()).unwrap_or(p) };
                            let _ = open(&p.to_string_lossy(), true);
                        }
                        let _ = crate::menus::invoke(self, ctx, "help.reportIssue", serde_json::json!({}));
                    }
                    let d = ui.button(tl!("Continue"));
                    self.auto.add("error.dismiss", d.rect, "Continue");
                    close |= d.clicked();
                });
            });
        if close {
            self.ui_error = None;
        }
    }
}

/// How long a closing app waits for its background jobs, and then for its frame workers, to stop.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

impl Drop for FilmcraftApp {
    /// Stop the session's background jobs (the render cache, proxies) and the frame workers, and
    /// wait briefly for them: their work is cancelled, not finished. Left running, a closed app's
    /// work kept rendering for seconds, holding its frames and media in memory; the UI test
    /// binary (one app per test) piled them up until it ran out of memory (fork, 2026-10-13).
    fn drop(&mut self) {
        self.tl.cancel_peak_jobs();
        if !self.session.background.stop_and_wait(CLOSE_WAIT) {
            log::warn!("closing: a background job did not stop within {CLOSE_WAIT:?}");
        }
        if !self.frames.stop_and_wait(CLOSE_WAIT) {
            log::warn!("closing: a frame worker did not stop within {CLOSE_WAIT:?}");
        }
    }
}

impl eframe::App for FilmcraftApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.native_window_focused = Some(raw_input.focused);
        if self.control_input_only {
            // Preserve screenshot responses and all viewport lifecycle/geometry metadata.
            raw_input.events.retain(|event| matches!(event, egui::Event::Screenshot { .. }));
            raw_input.hovered_files.clear();
            raw_input.dropped_files.clear();
        }
        if self.control_input_only || !self.synthetic.is_empty() {
            // Native modifiers cannot replace those requested by a control gesture. A queued
            // reset runs in the frame after release, so clicked() still sees Shift/Alt.
            raw_input.events.push(egui::Event::ModifiersChanged(self.synthetic_modifiers));
            // Widget keyboard handling requires effective egui focus. Control input must work
            // in a background window without activating it or taking the user's OS keyboard.
            raw_input.focused = true;
        }
        if !self.synthetic.is_empty() {
            // Pointer events go one per frame so egui sees press → moves → release as a real drag
            // (all in one frame reads as a click); key/text runs go together up to a key release.
            let pointer = |e: &egui::Event| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. });
            let n = if pointer(&self.synthetic[0]) {
                1
            } else {
                self.synthetic
                    .iter()
                    .position(|e| pointer(e) || matches!(e, egui::Event::Key { pressed: false, .. }))
                    .map_or(self.synthetic.len(), |i| if pointer(&self.synthetic[i]) { i.max(1) } else { i + 1 })
            };
            for event in self.synthetic.drain(..n) {
                if let egui::Event::ModifiersChanged(modifiers) = event {
                    self.synthetic_modifiers = modifiers;
                }
                raw_input.events.push(event);
            }
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // a native file dialog's answer first, so the panels draw what it chose (#260)
        self.poll_native_dialog(ctx);
        if !self.styled {
            theme::install(ctx, &self.tokens);
            // theme::install replaces the fonts: add the system Japanese font back (or fall back to
            // English when a saved Japanese setting meets a system without one)
            // (likewise Chinese without a Chinese face)
            let japanese_missing = self.ui.language == i18n::Language::Ja && !i18n::install_japanese_font(ctx);
            if japanese_missing || (self.ui.language == i18n::Language::ZhCn && !i18n::chinese_font_available()) {
                self.ui.language = i18n::Language::En;
            }
            self.styled = true;
            ctx.request_repaint();
        } else {
            self.fonts_ready = true;
        }
        let now = ctx.input(|i| i.time);
        let dt = (now - self.last_time) as f32;
        if dt > 0.0 {
            self.fps = self.fps * 0.9 + (1.0 / dt).min(480.0) * 0.1;
        }
        self.last_time = now;
        if self.playback.playing && ctx.input(|i| i.viewport().visible()) == Some(false) {
            // Nothing is shown while the window is hidden: not a dropped frame.
            self.playback.hidden = true;
        }
        self.timeline_still = if self.ui.timeline.animating() { 0 } else { self.timeline_still.saturating_add(1) };
        // Fork L1.5: a file dialog parented to the window is still open (its thread holds the
        // window's Wayland export): it is closed first, so the window isn't torn down under it.
        // Quitting with unsaved changes asks to save them first.
        if ctx.input(|i| i.viewport().close_requested()) && self.close_waits_for_dialog() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.ui.status = tl!("Close the file dialog first").into();
        } else if ctx.input(|i| i.viewport().close_requested()) && panels::clip_dialogs::intercept_quit(self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let had_synthetic = !self.synthetic.is_empty();
        self.drain_control(ctx);
        if !self.synthetic.is_empty() && !had_synthetic {
            // Occluded macOS windows stop running `ui`; bring the window forward (without taking
            // keyboard focus) so the input is processed.
            self.raise_for_control(ctx);
        }
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        } else if !self.input_waiters.is_empty() {
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        self.desktop_tick(ctx);
        let settled = !std::mem::take(&mut self.monitor_inexact) && self.timeline_still > 0;
        self.issue_screenshots(ctx, settled);
        self.collect_screenshots(ctx);
    }

    fn on_exit(&mut self) {
        // a project opened from a remote location gives its lock back (STORAGE-PLAN S2)
        let _ = self.session.execute("remote.closeProject", serde_json::json!({}));
        // taskbar progress, inhibitors and the single-instance name go first (`desktop`)
        if let Some(host) = self.hooks.desktop.as_mut() {
            host.shutdown();
        }
        // Flush the recovery journal and stop the auto-save worker; a session with nothing unsaved
        // removes its journal, one with unsaved changes keeps it for the next launch.
        self.session.shutdown();
        // Restart Now (fork UPD): the new build starts after this one has let go of everything
        panels::update_prompt::spawn_restart(self);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.fonts_ready {
            ui.ctx().request_repaint();
            return;
        }
        // No frame worker threads on the web: render queued frames here, within a time budget
        // that leaves room for the UI pass (a no-op where workers run).
        self.frames.pump(std::time::Duration::from_millis(if self.playback.playing { 24 } else { 40 }));
        // A panic in one panel must not close the app (losing unsaved work) or leave a blank
        // window: catch it, keep the session, and say what happened.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.frame(ui);
            window_chrome::show(self, ui);
        }))
        .is_err()
        {
            self.ui_error = Some(crash::take_last().unwrap_or_else(|| "unknown error".into()));
            self.playback.playing = false;
        }
        self.error_window(ui.ctx());
        let ctx = ui.ctx().clone();
        self.last_ui_time = ctx.input(|i| i.time);
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        } else if !self.input_waiters.is_empty() {
            ctx.request_repaint();
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        if self.frames.queue_len() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

/// The first half of the launch (fork RS2): open the project named on the command line, else
/// follow `--demo` / `--empty`, else Settings ▸ General ▸ At startup show (Last project opens the
/// most recent project that still exists, else the demo project). Returns the startup choice
/// (None when a project was named); [`FilmcraftApp::finish_launch`] shows the home screen for it.
pub fn launch_project(session: &mut Session, project: Option<&str>, startup_flag: bool, demo: bool) -> Option<filmcraft_engine::projects::StartupAction> {
    use filmcraft_engine::projects::StartupAction;
    if let Some(p) = project {
        if let Err(e) = session.execute("file.open", json!({"path": p})) {
            log::error!("{e}");
        }
        return None;
    }
    let action = filmcraft_engine::projects::startup_action(&session.prefs, startup_flag, demo);
    match action {
        StartupAction::LastProject => {
            let recent = session.prefs.general.recent_projects.iter().find(|p| std::path::Path::new(p).exists()).cloned();
            // (as before RS2: a recent project that fails to open is logged and leaves an empty
            // project; with none left the demo project opens)
            match recent.map(|p| session.execute("file.open", json!({"path": p}))) {
                Some(Ok(_)) => {}
                Some(Err(e)) => log::error!("{e}"),
                None => {
                    let _ = session.execute("file.openDemoProject", json!({}));
                }
            }
        }
        StartupAction::Demo => {
            let _ = session.execute("file.openDemoProject", json!({}));
        }
        StartupAction::NewEmpty | StartupAction::ProjectManager => {}
    }
    Some(action)
}

/// The program mix that playback plays: `mix(device_frame, interleaved, channels)` renders the
/// active sequence at device rate `sr` from `device_frame`, with the live project snapshot (edits
/// made while playing are heard), Output Mapping, the 5.1 mixdown and voice-over cues.
pub fn playback_mix(session: &Session, seq_id: filmcraft_project::ItemId, sr: u32) -> impl FnMut(i64, &mut [f32], usize) + Send + 'static {
    let project = session.project.clone();
    let provider = session.media.provider(project.clone(), session.services.clone());
    let previews = session.previews.clone();
    // Settings ▸ Audio Hardware ▸ Output Mapping
    let map = [session.prefs.audio_hardware.map_left, session.prefs.audio_hardware.map_right];
    // Preferences ▸ Audio ▸ 5.1 Mixdown Type: how a 5.1 Mix plays on a stereo device
    let mixdown = filmcraft_audio_dsp::channels::Mixdown::from_id(&session.prefs.audio.mixdown_type).unwrap_or_default();
    let cues = panels::voiceover::cues(session, sr);
    previews.live.publish_project(project.clone());
    let mut resampler = None;
    move |cursor: i64, buf: &mut [f32], ch: usize| {
        // the newest project snapshot: mixer moves and other edits are heard while playing
        let project = previews.live.project().filter(|p| p.sequence(seq_id).is_some()).unwrap_or_else(|| project.clone());
        let Some(seq) = project.sequence(seq_id) else {
            buf.fill(0.0);
            return;
        };
        let n = buf.len() / ch.max(1);
        let seq_sr = seq.settings.sample_rate;
        // a 5.1 Mix plays as six channels (L, R, C, LFE, Ls, Rs) on a device with at least six
        use filmcraft_audio_dsp::channels::Layout;
        let layout = if ch >= 6 && seq.settings.audio_master == filmcraft_project::AudioChannels::Surround51 { Layout::Surround51 } else { Layout::Stereo };
        let mix = if seq_sr == sr {
            previews.mix_layout(&project, seq_id, cursor, n, &provider, layout, mixdown)
        } else {
            // Mix at the sequence rate as one continuous stream, interpolated to the device rate
            // (started afresh when the rate or the channel layout changes while playing).
            let key = (seq_sr, layout);
            if resampler.as_ref().is_none_or(|(k, _)| *k != key) {
                resampler = Some((key, filmcraft_audio_dsp::resample::StreamResampler::new(seq_sr, sr)));
            }
            let Some((_, r)) = resampler.as_mut() else {
                buf.fill(0.0);
                return;
            };
            let channels = r.process(cursor, n, |s0, m| previews.mix_layout(&project, seq_id, s0, m, &provider, layout, mixdown).channels);
            filmcraft_frame::AudioBuffer { sample_rate: sr, channels }
        };
        // (checked access: a mix with fewer channels or frames than asked for plays silence there
        // instead of panicking the mixer, fork A1.2)
        if layout == Layout::Surround51 {
            buf.fill(0.0);
            for (i, frame) in buf.chunks_mut(ch.max(1)).enumerate() {
                for (c, x) in frame.iter_mut().take(6).enumerate() {
                    *x = mix.channels.get(c).and_then(|s| s.get(i)).copied().unwrap_or(0.0);
                }
            }
        } else {
            let left = mix.channels.first().map_or(&[][..], Vec::as_slice);
            let right = mix.channels.get(1).map_or(left, Vec::as_slice);
            filmcraft_engine::settings::map_output(left, right, buf, ch, map);
        }
        panels::voiceover::mix_cues(buf, ch, cursor, &cues);
    }
}

/// The windowing layer (egui-winit, and egui's web backend) reports the clipboard shortcuts as
/// `Event::Copy` / `Event::Cut` / `Event::Paste` instead of key presses: Ctrl+C/X/V on Windows and
/// Linux, plus Ctrl+Insert, Shift+Insert and Shift+Delete on Windows. The shortcut bindings only
/// match key presses, so Copy, Cut, Paste, Paste Insert, Paste Attributes and (on Windows) Ripple
/// Delete never fired from the keyboard (#199). macOS was unaffected because its native menu bar
/// takes the key equivalents first.
///
/// Add the key press each event stands for, with the modifiers held, so the bindings see it. The
/// original event stays for anything else that reads it. Only called when no text field has
/// keyboard focus; text fields keep handling the clipboard themselves.
fn clipboard_events_as_keys(events: &mut Vec<egui::Event>, modifiers: egui::Modifiers) {
    use egui::{Event, Key};
    let keys: Vec<Key> = events
        .iter()
        .filter_map(|e| match e {
            Event::Copy if modifiers.command => Some(Key::C),
            Event::Copy => Some(Key::Copy),
            Event::Cut if modifiers.command => Some(Key::X),
            Event::Cut if modifiers.shift => Some(Key::Delete),
            Event::Cut => Some(Key::Cut),
            Event::Paste(_) if modifiers.command => Some(Key::V),
            Event::Paste(_) if modifiers.shift => Some(Key::Insert),
            Event::Paste(_) => Some(Key::Paste),
            _ => None,
        })
        .collect();
    events.extend(keys.into_iter().map(|key| Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }));
}

#[cfg(test)]
mod clipboard_key_tests {
    use egui::{Event, Key, Modifiers};

    fn keys(events: Vec<Event>, m: Modifiers) -> Vec<(Key, Modifiers)> {
        let mut events = events;
        super::clipboard_events_as_keys(&mut events, m);
        events.into_iter().filter_map(|e| if let Event::Key { key, modifiers, pressed: true, .. } = e { Some((key, modifiers)) } else { None }).collect()
    }

    #[test]
    fn ctrl_c_x_v_become_key_presses() {
        let c = Modifiers::COMMAND;
        assert_eq!(keys(vec![Event::Copy], c), vec![(Key::C, c)]);
        assert_eq!(keys(vec![Event::Cut], c), vec![(Key::X, c)]);
        assert_eq!(keys(vec![Event::Paste("x".into())], c), vec![(Key::V, c)]);
    }

    #[test]
    fn extra_modifiers_are_kept() {
        // Paste Insert (Ctrl+Shift+V) and Paste Attributes (Ctrl+Alt+V) also arrive as Paste
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(keys(vec![Event::Paste("x".into())], cs), vec![(Key::V, cs)]);
        let ca = Modifiers::COMMAND | Modifiers::ALT;
        assert_eq!(keys(vec![Event::Paste("x".into())], ca), vec![(Key::V, ca)]);
    }

    #[test]
    fn windows_insert_and_delete_variants() {
        // Shift+Delete (Ripple Delete) arrives as Cut on Windows; Shift+Insert as Paste
        assert_eq!(keys(vec![Event::Cut], Modifiers::SHIFT), vec![(Key::Delete, Modifiers::SHIFT)]);
        assert_eq!(keys(vec![Event::Paste("x".into())], Modifiers::SHIFT), vec![(Key::Insert, Modifiers::SHIFT)]);
    }

    #[test]
    fn other_events_are_left_alone() {
        let mut events = vec![Event::Text("a".into()), Event::Copy];
        super::clipboard_events_as_keys(&mut events, Modifiers::COMMAND);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], Event::Text(_)));
        assert!(matches!(events[1], Event::Copy));
    }
}

#[cfg(test)]
mod gpu_fallback_tests {
    use filmcraft_render::plan::{FramePlan, PlanLayer};

    /// Software rasterizers must not be treated as a GPU compositor: the preview crawled while
    /// "GPU" usage read 0 % because wgpu had silently fallen back to llvmpipe.
    #[test]
    fn software_rasterizers_are_detected() {
        use eframe::wgpu::DeviceType as D;
        let r = super::rasterizer_reason;
        assert!(r("llvmpipe (LLVM 23.1.1, 256 bits)", D::Cpu).is_some());
        assert!(r("NVIDIA GeForce RTX 4080", D::Cpu).is_some());
        assert!(r("SwiftShader Device", D::Other).is_some());
        assert!(r("softpipe", D::Cpu).is_some());
        assert!(r("AMD Radeon RX 5700 XT", D::DiscreteGpu).is_none());
        assert!(r("Apple M3 Pro", D::IntegratedGpu).is_none());
    }

    #[test]
    fn plan_side_covers_output_and_every_layer() {
        let frame = |w: u32, h: u32| std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba_f32(w, h, vec![0.0; (w * h * 4) as usize]));
        let layer = |w, h| PlanLayer {
            frame: frame(w, h),
            matrix: filmcraft_geom::Affine::IDENTITY,
            opacity: 1.0,
            blend: filmcraft_render::Blend::Normal,
            fx: None,
            comp: None,
        };
        // a 4K source in an HD sequence still needs a 3840-wide texture on the GPU
        let p = FramePlan::Layers { output: None, width: 1920, height: 1080, layers: vec![layer(64, 64), layer(3840, 2160)] };
        assert_eq!(super::plan_side(&p), 3840);
        let p = FramePlan::Layers { output: None, width: 1920, height: 1080, layers: vec![] };
        assert_eq!(super::plan_side(&p), 1920);
        let p = FramePlan::Image(filmcraft_render::Image::new(800, 4000));
        assert_eq!(super::plan_side(&p), 4000);
    }
}

#[cfg(test)]
mod audio_recovery_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Device {
        ready: bool,
        starts: Arc<AtomicUsize>,
        reading: Option<u64>,
    }

    impl AudioOut for Device {
        fn sample_rate(&self) -> u32 {
            48000
        }
        fn channels(&self) -> usize {
            2
        }
        fn configure(&mut self, hardware: &filmcraft_engine::settings::AudioHardwarePrefs, _: Option<u32>) {
            self.ready = hardware.default_output == "available";
        }
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            self.starts.fetch_add(1, Ordering::Relaxed);
            if self.ready { Ok(48000) } else { Err("device unavailable".into()) }
        }
        fn stop(&mut self) {}
        fn played_frames(&self) -> Option<u64> {
            self.reading
        }
    }

    /// A device whose rate moves when it starts (the default sink changed since `configure`):
    /// each start opens at `rates[n]` and reports it from then on.
    struct Moving {
        rates: Vec<u32>,
        rate: u32,
        starts: Arc<AtomicUsize>,
        mixed_at: Arc<std::sync::Mutex<Vec<u32>>>,
    }

    impl AudioOut for Moving {
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            // the mix handed in was built at the rate reported before this start
            self.mixed_at.lock().unwrap().push(self.rate);
            let n = self.starts.fetch_add(1, Ordering::Relaxed);
            self.rate = self.rates.get(n).copied().unwrap_or(self.rate);
            Ok(self.rate)
        }
        fn stop(&mut self) {}
        fn played_frames(&self) -> Option<u64> {
            Some(0)
        }
    }

    /// Fork A1.3b: a device that opens at another rate than it reported gets the mix rebuilt at
    /// the rate it really plays, once; one that keeps moving isn't chased: no sound, wall clock.
    #[test]
    fn a_device_that_opens_at_another_rate_gets_a_mix_at_that_rate() {
        for (rates, starts_wanted, last_mix, sound) in
            [(vec![44_100, 44_100], 2, 44_100, true), (vec![48_000], 1, 48_000, true), (vec![44_100, 96_000, 22_050], 2, 44_100, false)]
        {
            let mut session = Session::default();
            session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
            let mut app = FilmcraftApp::new(session);
            let starts = Arc::new(AtomicUsize::new(0));
            let mixed_at = Arc::new(std::sync::Mutex::new(Vec::new()));
            app.audio = Some(Box::new(Moving { rates: rates.clone(), rate: 48_000, starts: starts.clone(), mixed_at: mixed_at.clone() }));
            app.play(1.0);
            app.end_preroll(0.0);
            assert_eq!(starts.load(Ordering::Relaxed), starts_wanted, "{rates:?}");
            assert_eq!(mixed_at.lock().unwrap().last().copied(), Some(last_mix), "{rates:?}");
            // a device that keeps moving plays on the wall clock without sound, and says so
            assert_eq!(app.playback.audio_clock, sound, "{rates:?}");
            assert_eq!(app.ui.status.contains("keeps changing its sample rate"), !sound, "{rates:?}: {}", app.ui.status);
            app.stop();
        }
    }

    /// Fork A1.2: an output that can't start says so in the status line (it used to only log).
    #[test]
    fn an_audio_output_that_cannot_start_says_so() {
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.audio = Some(Box::new(Device { ready: false, starts: Arc::default(), reading: Some(0) }));
        app.play(1.0);
        app.end_preroll(0.0);
        assert!(!app.playback.audio_clock);
        assert!(app.ui.status.contains("could not start") && app.ui.status.contains("device unavailable"), "{}", app.ui.status);
        app.stop();
    }

    /// Fork A1.2: a mixer that stopped after repeated panics hands playback to the wall clock and
    /// says so once, instead of the device clock running on over silence.
    #[test]
    fn a_stopped_mixer_hands_over_to_the_wall_clock_and_says_so() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        app.audio = Some(Box::new(Device { ready: true, starts: Arc::default(), reading: Some(4_800) }));
        app.playback.playing = true;
        app.playback.audio_clock = true;
        app.playback.anchor_tick = Tick::ZERO;
        app.playback.anchor_time = 0.0;
        app.playback.audio_seen = (0, 0.0);
        app.playback.audio_stats.mixer_stopped.store(true, Ordering::Relaxed);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(1.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(!app.playback.audio_clock);
        assert!(app.ui.status.contains("Audio mixing stopped"), "{}", app.ui.status);
        app.stop();
    }

    #[test]
    fn forward_shuttle_playback_keeps_audio_enabled() {
        // (2x is within `shuttle_audio::audible`: varispeed, tape-style)
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        let mut app = FilmcraftApp::new(session);
        let starts = Arc::new(AtomicUsize::new(0));
        app.audio = Some(Box::new(Device { ready: true, starts: starts.clone(), reading: Some(0) }));
        app.play(2.0);
        app.end_preroll(0.0);
        assert_eq!(starts.load(Ordering::Relaxed), 1, "2x playback must start audio output");
        assert!(app.playback.audio_clock, "the device is still the master clock");
        app.stop();
    }

    #[test]
    fn changing_hardware_recovers_wall_clock_playback_without_rewinding() {
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        let starts = Arc::new(AtomicUsize::new(0));
        app.audio = Some(Box::new(Device { ready: false, starts: starts.clone(), reading: Some(0) }));
        app.apply_prefs(&ctx);
        app.play(1.0);
        app.end_preroll(0.0);
        assert!(!app.playback.audio_clock);
        app.session.set_playhead(Tick::from_seconds_f64(2.0));
        app.session.prefs.audio_hardware.default_output = "available".into();
        app.apply_prefs(&ctx);
        assert_eq!(starts.load(Ordering::Relaxed), 2);
        assert!(app.playback.audio_clock);
        assert_eq!(app.playback.anchor_tick, app.session.playhead());
        app.stop();
    }

    #[test]
    fn a_failed_device_clock_resumes_from_the_displayed_frame() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        app.audio = Some(Box::new(Device { ready: true, starts: Arc::new(AtomicUsize::new(0)), reading: None }));
        let displayed = app.session.sequence_rate().tick_of(48);
        app.session.set_playhead(displayed);
        app.playback.playing = true;
        app.playback.audio_clock = true;
        app.playback.anchor_tick = Tick::ZERO;
        app.playback.anchor_time = 0.0;
        let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(!app.playback.audio_clock);
        assert_eq!(app.playback.anchor_tick, displayed);
        assert_eq!(app.session.playhead(), displayed);
        assert!(app.ui.status.contains("Audio output failed"));
        app.stop();
    }

    /// A stream the device dropped, which `recover` restarts on the same clock.
    struct Restartable {
        reading: Option<u64>,
    }

    impl AudioOut for Restartable {
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            Ok(48000)
        }
        fn stop(&mut self) {}
        fn sample_rate(&self) -> u32 {
            48000
        }
        fn played_frames(&self) -> Option<u64> {
            self.reading
        }
        fn recover(&mut self) {
            self.reading = self.reading.or(Some(48_000));
        }
    }

    #[test]
    fn a_restarted_stream_keeps_the_device_clock_and_the_sound() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        app.audio = Some(Box::new(Restartable { reading: None }));
        app.playback.playing = true;
        app.playback.audio_clock = true;
        app.playback.anchor_tick = Tick::ZERO;
        app.playback.anchor_time = 0.0;
        app.playback.audio_seen = (0, 10.0);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(app.playback.audio_clock, "the restarted stream is still the master clock");
        assert!(!app.ui.status.contains("Audio output failed"));
        // 48 000 frames played at 48 kHz: one second in, on a frame boundary
        let (shown, second) = (app.session.playhead(), Tick::from_seconds_f64(1.0));
        assert!(
            shown <= second && shown + app.session.sequence_rate().frame_duration() > second,
            "the playhead follows the frames the device played: {shown:?}"
        );
        app.stop();
    }
}

/// A physical-pixel screen position as a point in the window's UI space, given the window's content origin in points.
///
/// Assumes the whole virtual screen uses the window's `pixels_per_point`: with monitors of mixed
/// DPI the point can be off when the cursor and the window's origin are on different monitors.
fn screen_px_to_ui(px: (i32, i32), pixels_per_point: f32, content_min: egui::Pos2) -> egui::Pos2 {
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
    egui::pos2(px.0 as f32 / ppp - content_min.x, px.1 as f32 / ppp - content_min.y)
}

#[cfg(test)]
mod screen_px_tests {
    use super::screen_px_to_ui;

    #[test]
    fn maps_screen_pixels_into_the_window() {
        let p = screen_px_to_ui((1300, 900), 2.0, egui::pos2(100.0, 50.0));
        assert_eq!((p.x, p.y), (550.0, 400.0));
    }

    #[test]
    fn a_hostile_scale_does_not_divide_by_zero() {
        for ppp in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let p = screen_px_to_ui((10, 20), ppp, egui::pos2(0.0, 0.0));
            assert!(p.x.is_finite() && p.y.is_finite());
        }
    }
}
