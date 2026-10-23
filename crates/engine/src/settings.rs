//! **Settings** (Premiere: app menu ▸ Settings ▸ <category>; the window is titled "Preferences").
//!
//! The values live in [`crate::autosave::Preferences`] (persisted as `preferences.json` in the
//! per-user data directory, never in the project). Every value is a dotted camelCase key whose first
//! segment is the category id (`timeline.stillImageDuration`, `labels.colors.violet.name`), so the
//! `prefs.get` / `prefs.set` / `prefs.reset` commands reach all of them from the CLI, the control
//! channel and MCP, and the Settings dialog's controls carry the automation id `settings.<key>`.
//!
//! This module holds the categories' value types, the field schema the dialog is drawn from
//! ([`categories`]; also returned by `prefs.schema` so agents can discover choices and ranges),
//! validation/migration, and the engine-side helpers that make the values take effect (default
//! durations, label colours, the media cache, audio output mapping, smart quotes…).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use filmcraft_project::Label;
use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

/// Version of the preferences file layout ([`crate::autosave::Preferences::version`]).
/// v1: Auto Save, Audio (mixer automation), Playback (pre/postroll), Trim, Media (proxies), guides,
/// Essential Sound presets. v2: every Settings category. v3: fork RS1 (Resolve as the default
/// workflow; only the fork build acts on files older than v3, upstream just carries the number).
pub const PREFS_VERSION: u32 = 3;

// ------------------------------------------------------------------ category values

/// Settings ▸ Workflow: the editor whose conventions the keyboard follows. Premiere is the
/// FilmCraft default; Resolve loads the "DaVinci Resolve Compatible" keyboard preset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Workflow {
    #[cfg_attr(not(feature = "fork-resolve-default"), default)] // FORK-ONLY
    Premiere,
    #[cfg_attr(feature = "fork-resolve-default", default)] // FORK-ONLY (RESOLVE-UI-PLAN RS1)
    Resolve,
}

impl Workflow {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "premiere" => Some(Self::Premiere),
            "resolve" => Some(Self::Resolve),
            _ => None,
        }
    }
}

/// Settings ▸ General.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralPrefs {
    /// Interface language (persisted independently of projects): `en`, `ja`, `es`, `pt-br`, `uk`,
    /// `zh-cn`, `de`, `ru`, `fr`, or `system` (the default: the operating system's preferred language when the
    /// interface has it, otherwise English).
    pub interface_language: String,
    /// "At Startup": `showHome` (FilmCraft: the demo project), `openMostRecent`, `emptyProject`.
    pub at_startup: String,
    /// "When Opening a Project": `showOpenDialog` | `showHome`.
    pub when_opening_project: String,
    /// Bins ▸ Double-click / + Cmd / + Opt: `openInPlace` | `openNewTab` | `openNewWindow`.
    pub bins_double_click: String,
    pub bins_cmd_double_click: String,
    pub bins_opt_double_click: String,
    /// Projects ▸ Double-click / + Opt.
    pub projects_double_click: String,
    pub projects_opt_double_click: String,
    pub show_event_indicator: bool,
    pub show_tool_tips: bool,
    pub show_rich_tool_tips: bool,
    pub show_workspace_reset_warning: bool,
    pub show_project_load_errors: bool,
    pub show_compatibility_issues: bool,
    pub show_project_bin_in_tab: bool,
    pub show_mask_tracker_preview: bool,
    /// Most recently opened / saved projects (newest first; not shown in the dialog).
    pub recent_projects: Vec<String>,
    /// "At startup show" (fork RS2): Project Manager, Last project or New empty project; `None`
    /// (`"workflowDefault"`) = the Project Manager in the Resolve workflow, At Startup in Premiere.
    #[serde(with = "crate::projects::startup_show_serde")]
    pub startup_show: Option<crate::projects::StartupShow>,
    /// "Projects folder" of the Project Manager home screen (fork RS2); `None` (`""`) =
    /// `~/Videos/FilmCraft Projects`, created on first use.
    #[serde(with = "crate::projects::folder_serde")]
    pub projects_folder: Option<PathBuf>,
    /// "Window title bar" (fork): `filmcraft` = FilmCraft draws its own title bar (window
    /// buttons and resize edges, like DaVinci Resolve) on Linux and Windows; `system` = the
    /// desktop's title bar (KWin, Mutter, Windows). macOS always keeps its own traffic lights.
    pub title_bar: String,
    /// "Full screen" (fork, owner 2026-10-13): the window covers the whole screen, the desktop's
    /// panels and task bar included, like DaVinci Resolve. On by default; the window's full-screen
    /// button, View ▸ Full Screen, F11 and `window.toggleFullScreen` switch it and are remembered.
    pub full_screen: bool,
    /// "Project Manager full screen" (fork, owner 2026-10-13): the Project Manager remembers its own
    /// full-screen state, as in Resolve. Off by default: it floats in a window, and opening a
    /// project switches to the editor's state ([`Self::full_screen`]).
    pub project_manager_full_screen: bool,
    /// "Menu bar" (fork, owner 2026-10-13): `auto` = in the desktop's global menu when it shows one
    /// (Plasma's Global Menu, the macOS-style top panel) and in the window otherwise, and always in
    /// the window in full screen (the panel is covered); `window` = always in the window.
    pub menu_bar: String,
}

impl Default for GeneralPrefs {
    fn default() -> Self {
        Self {
            interface_language: "system".into(),
            at_startup: "showHome".into(),
            when_opening_project: "showOpenDialog".into(),
            bins_double_click: "openInPlace".into(),
            bins_cmd_double_click: "openNewTab".into(),
            bins_opt_double_click: "openNewWindow".into(),
            projects_double_click: "openNewTab".into(),
            projects_opt_double_click: "openNewWindow".into(),
            show_event_indicator: true,
            show_tool_tips: true,
            show_rich_tool_tips: true,
            show_workspace_reset_warning: true,
            show_project_load_errors: true,
            show_compatibility_issues: true,
            show_project_bin_in_tab: true,
            show_mask_tracker_preview: true,
            recent_projects: Vec::new(),
            startup_show: None,
            projects_folder: None,
            title_bar: "filmcraft".into(),
            full_screen: true,
            project_manager_full_screen: false,
            menu_bar: "auto".into(),
        }
    }
}

/// Settings ▸ Appearance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppearancePrefs {
    /// Legacy single "Color Theme": `darkest` | `dark` | `light`. Setting it (old settings files,
    /// automation, View ▸ Appearance) selects that theme and fixes the mode to its family. It is
    /// kept in line with the fields below ([`AppearancePrefs::sync_color_theme`]): the light or
    /// dark theme of a fixed mode, and the dark theme in Auto (whose shown theme depends on the
    /// system; read `appearanceMode` for that).
    pub color_theme: String,
    /// "Appearance Mode": `auto` (follow the operating system) | `dark` (default) | `light`.
    pub appearance_mode: String,
    /// The theme shown in dark mode: `darkest` (default) | `dark` | `resolve` (the Resolve
    /// workflow's own look).
    pub dark_theme: String,
    /// The theme shown in light mode: `light`.
    pub light_theme: String,
    /// "Accessible color contrast": brighter secondary text and borders.
    pub accessible_contrast: bool,
    /// Highlight (accent) colour of selections, focus and primary buttons (`#rrggbb`).
    pub highlight_color: String,
    /// "Use the system accent color" (L1.4): the desktop's accent colour (XDG desktop portal
    /// `accent-color`; KDE Plasma, GNOME 47+) replaces the highlight colour while the desktop
    /// reports one, and follows it when it changes. Off by default.
    pub follow_system_accent: bool,
    /// "Animations" (L1.4): `system` (default: off when the desktop asks for reduced motion) |
    /// `on` | `off` (menus, collapsing sections and scrolling jump instead of sliding).
    pub motion: String,
    /// "UI Scale": `auto` (default: the scale the operating system reports) or a fixed
    /// percentage of [`UI_SCALES`] (`100` = one point per physical pixel).
    pub ui_scale: String,
    /// "Match the desktop's text size" (L1.4, owner question Q15 option b): UI Scale ▸ Auto is
    /// multiplied by the desktop's text size (KDE's General font against its 10 pt default,
    /// GNOME's `text-scaling-factor`). The whole interface scales, not only text. Off by default;
    /// a fixed UI Scale ignores it.
    pub match_desktop_text_size: bool,
    /// "App icon" (fork, owner 2026-10-20, FORK-PLAN §1a.108): `rounded` (default: the macOS-style
    /// tile the packages install) | `square` (the plain square picture). The window icon follows
    /// at once; on Linux the desktop's launcher / taskbar icon is written to the user's icon theme.
    pub app_icon: String,
}

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            color_theme: "darkest".into(),
            appearance_mode: "dark".into(),
            dark_theme: "darkest".into(),
            light_theme: "light".into(),
            accessible_contrast: false,
            highlight_color: DEFAULT_HIGHLIGHT.into(),
            follow_system_accent: false,
            motion: "system".into(),
            ui_scale: "auto".into(),
            match_desktop_text_size: false,
            app_icon: "rounded".into(),
        }
    }
}

impl AppearancePrefs {
    /// The fixed UI scale (physical pixels per point) "UI Scale" asks for, or `None` for `auto`
    /// (and for a value outside [`UI_SCALES`]), which keeps the operating system's scale.
    pub fn ui_scale_factor(&self) -> Option<f32> {
        // `auto` doesn't parse; every other listed value is a positive whole percentage
        UI_SCALES.iter().any(|(k, _)| *k == self.ui_scale).then(|| self.ui_scale.parse::<f32>().ok()).flatten().map(|pct| pct / 100.0)
    }

    /// The zoom "Match the desktop's text size" adds on top of the system's scale: the desktop's
    /// text factor (`system_text`, already validated by the host) while UI Scale is Auto and the
    /// switch is on, else `None`. Hostile factors are kept inside [`TEXT_SCALE_RANGE`].
    pub fn text_scale_zoom(&self, system_text: Option<f32>) -> Option<f32> {
        if !self.match_desktop_text_size || self.ui_scale_factor().is_some() {
            return None;
        }
        let f = system_text.filter(|f| f.is_finite() && *f > 0.0)?;
        let f = f.clamp(*TEXT_SCALE_RANGE.start(), *TEXT_SCALE_RANGE.end());
        ((f - 1.0).abs() >= 0.01).then_some(f)
    }

    /// Whether animations are off: "Animations" `off`, or `system` (and any unknown value) while the
    /// desktop asks for reduced motion (`system_reduced`).
    pub fn reduced_motion(&self, system_reduced: bool) -> bool {
        match self.motion.as_str() {
            "off" => true,
            "on" => false,
            _ => system_reduced,
        }
    }

    /// The theme to show (`darkest` | `dark` | `light`): the light or dark theme of the mode. Auto
    /// follows `system_light` (the operating system's appearance) and is dark without an answer.
    pub fn shown_theme(&self, system_light: Option<bool>) -> &str {
        let light = match self.appearance_mode.as_str() {
            "light" => true,
            "auto" => system_light == Some(true),
            _ => false,
        };
        if light { &self.light_theme } else { &self.dark_theme }
    }

    /// Select `theme` (a Color Theme value) as the theme of its family and fix the mode to that
    /// family. Unknown names are ignored.
    pub fn select_theme(&mut self, theme: &str) {
        if LIGHT_THEMES.iter().any(|(v, _)| *v == theme) {
            self.light_theme = theme.into();
            self.appearance_mode = "light".into();
        } else if DARK_THEMES.iter().any(|(v, _)| *v == theme) {
            self.dark_theme = theme.into();
            self.appearance_mode = "dark".into();
        }
    }

    /// Bring the legacy `color_theme` in line with the mode and theme choices, so clients that
    /// still read it get the theme a fixed mode shows (the dark theme in Auto).
    pub fn sync_color_theme(&mut self) {
        let theme = self.shown_theme(None).to_string();
        self.color_theme = theme;
    }

    /// The mode after this one in the header button's cycle: Auto, Light, Dark, Auto…
    pub fn next_mode(&self) -> &'static str {
        match self.appearance_mode.as_str() {
            "auto" => "light",
            "light" => "dark",
            _ => "auto",
        }
    }
}

/// The Resolve workflow's theme (a dark theme).
pub const RESOLVE_THEME: &str = "resolve";

pub const DEFAULT_HIGHLIGHT: &str = "#2f6bdf";

/// Settings ▸ Audio Hardware (the cpal output on desktop).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioHardwarePrefs {
    /// "Device Class": the audio host (`CoreAudio`, `WASAPI`, `ALSA`…); empty = system default.
    pub device_class: String,
    /// "Default Input" / "Default Output": device names; empty = the system default device.
    pub default_input: String,
    pub default_output: String,
    /// "I/O Buffer Size" (samples).
    pub buffer_size: u32,
    /// "Sample Rate" (Hz).
    pub sample_rate: u32,
    /// "Attempt to force hardware to document sample rate": open the device at the sequence rate.
    pub force_document_rate: bool,
    /// Output Mapping: the device channels (0-based) the programme's left and right go to.
    pub map_left: u32,
    pub map_right: u32,
}

impl Default for AudioHardwarePrefs {
    fn default() -> Self {
        Self {
            device_class: String::new(),
            default_input: String::new(),
            default_output: String::new(),
            buffer_size: 512,
            sample_rate: 48_000,
            force_document_rate: false,
            map_left: 0,
            map_right: 1,
        }
    }
}

/// Settings ▸ Color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColorPrefs {
    pub display_color_management: bool,
    pub extended_dynamic_range: bool,
    /// HDR Graphics White (nits): `100` | `203` | `300`.
    pub hdr_graphics_white: String,
    /// The Grading Coach (fork GRADE-PLAN G2.12): the hints switch and the finished lessons.
    pub coach: crate::coach::CoachPrefs,
    /// Local AI explanations (fork GRADE-PLAN G2.11): the opt-in switch and the chosen model.
    pub ai: crate::ai_explain::AiPrefs,
    /// Which model tracks Magic Masks (fork G3, LO.6): Auto / Better / Faster / MobileSAM, as last
    /// picked on the Magic Mask tab. An unknown value loads as Auto.
    #[serde(deserialize_with = "de_mask_model")]
    pub magic_mask: crate::magic_mask::MaskModel,
}

/// A saved Magic Mask model, or Auto for anything else (hostile or newer preferences).
fn de_mask_model<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<crate::magic_mask::MaskModel, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_str().and_then(crate::magic_mask::MaskModel::parse).unwrap_or_default())
}

impl Default for ColorPrefs {
    fn default() -> Self {
        Self {
            display_color_management: false,
            extended_dynamic_range: false,
            hdr_graphics_white: "203".into(),
            coach: Default::default(),
            ai: Default::default(),
            magic_mask: Default::default(),
        }
    }
}

/// Settings ▸ Graphics (Text, Shapes, Closed Captions).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphicsPrefs {
    pub ligatures: bool,
    pub hindi_digits: bool,
    pub smart_quotes: bool,
    /// `ltr` | `rtl`.
    pub paragraph_direction: String,
    /// `miter` | `round` | `bevel`.
    pub text_line_join: String,
    pub text_miter_limit: f64,
    /// `butt` | `round` | `square`.
    pub text_line_cap: String,
    /// `allLines` | `perLine`.
    pub background_fill: String,
    pub default_font: String,
    pub missing_font_replacement: String,
    pub emoji_font: String,
    pub shape_line_join: String,
    pub shape_miter_limit: f64,
    pub shape_line_cap: String,
    pub caption_font: String,
    pub caption_monospace_only: bool,
    pub cea708_wide: bool,
}

impl Default for GraphicsPrefs {
    fn default() -> Self {
        Self {
            ligatures: true,
            hindi_digits: false,
            smart_quotes: true,
            paragraph_direction: "ltr".into(),
            text_line_join: "miter".into(),
            text_miter_limit: 2.5,
            text_line_cap: "butt".into(),
            background_fill: "allLines".into(),
            default_font: "Inter".into(),
            missing_font_replacement: "Inter".into(),
            emoji_font: String::new(),
            shape_line_join: "miter".into(),
            shape_miter_limit: 2.5,
            shape_line_cap: "butt".into(),
            caption_font: "JetBrains Mono".into(),
            caption_monospace_only: true,
            cea708_wide: false,
        }
    }
}

/// One editable label colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LabelColor {
    pub name: String,
    /// `#rrggbb`.
    pub color: String,
}

impl Default for LabelColor {
    fn default() -> Self {
        Self { name: String::new(), color: "#808080".into() }
    }
}

/// Settings ▸ Labels ▸ Label Defaults (which label new items get).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LabelDefaults {
    pub movie: Label,
    pub video: Label,
    pub audio: Label,
    pub still: Label,
    pub sequence: Label,
    pub dynamic_link: Label,
    pub bin: Label,
    pub captions: Label,
}

impl Default for LabelDefaults {
    fn default() -> Self {
        Self {
            movie: Label::Iris,
            video: Label::Violet,
            audio: Label::Caribbean,
            still: Label::Lavender,
            sequence: Label::Forest,
            dynamic_link: Label::Rose,
            bin: Label::Mango,
            captions: Label::Mango,
        }
    }
}

/// Settings ▸ Labels: names and colours of the 16 labels (keyed by the label's id, e.g.
/// `labels.colors.violet.name`) and the label defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LabelPrefs {
    pub colors: BTreeMap<String, LabelColor>,
    pub defaults: LabelDefaults,
}

impl Default for LabelPrefs {
    fn default() -> Self {
        Self { colors: Label::ALL.iter().map(|l| (label_id(*l), default_label_color(*l))).collect(), defaults: LabelDefaults::default() }
    }
}

/// The key of a label in [`LabelPrefs::colors`] (`violet`, `iris`, …).
pub fn label_id(l: Label) -> String {
    l.name().to_ascii_lowercase()
}

fn default_label_color(l: Label) -> LabelColor {
    LabelColor { name: l.name().into(), color: hex(l.rgb()) }
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `#rrggbb` (or `rrggbb`) → bytes.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([p(0)?, p(2)?, p(4)?])
}

impl LabelPrefs {
    /// The colour of label `l` (the user's, else the built-in one).
    pub fn rgb(&self, l: Label) -> [u8; 3] {
        self.colors.get(&label_id(l)).and_then(|c| parse_hex(&c.color)).unwrap_or_else(|| l.rgb())
    }
    /// The display name of label `l`.
    pub fn name(&self, l: Label) -> String {
        self.colors.get(&label_id(l)).map(|c| c.name.trim()).filter(|n| !n.is_empty()).unwrap_or(l.name()).to_string()
    }
    /// The label a new media item of `kind` gets.
    pub fn for_media(&self, kind: filmcraft_media::MediaKind, has_video: bool, has_audio: bool) -> Label {
        use filmcraft_media::MediaKind as K;
        match kind {
            K::AudioOnly => self.defaults.audio,
            K::Still | K::ImageSequence => self.defaults.still,
            _ if has_video && !has_audio => self.defaults.video,
            _ if !has_video && has_audio => self.defaults.audio,
            _ => self.defaults.movie,
        }
    }
    pub(crate) fn sanitize_labels(&mut self) {
        for l in Label::ALL {
            let c = self.colors.entry(label_id(l)).or_insert_with(|| default_label_color(l));
            if parse_hex(&c.color).is_none() {
                c.color = hex(l.rgb());
            }
            c.name = c.name.chars().take(64).collect();
        }
        self.colors.retain(|k, _| Label::ALL.iter().any(|l| label_id(*l) == *k));
    }
}

/// Settings ▸ Media Analysis & Transcription.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaAnalysisPrefs {
    /// "Cache analysis results for re-use": `mediaCache` | `project`.
    pub cache_results: String,
    pub analyze_imported_media: bool,
    /// "Automatically transcribe clips".
    pub auto_transcribe: bool,
    /// `allImported` (transcribe on import) | `sequenceClips` (only clips edited into sequences).
    pub auto_transcribe_scope: String,
    /// "Speaker Labeling": `on` | `off`.
    pub speaker_labeling: String,
    pub language_auto_detect: bool,
    /// "Default language" (ISO 639-1).
    pub default_language: String,
    /// Speech model (`transcript.models`).
    pub whisper_model: String,
}

impl Default for MediaAnalysisPrefs {
    fn default() -> Self {
        Self {
            cache_results: "mediaCache".into(),
            analyze_imported_media: true,
            auto_transcribe: false,
            auto_transcribe_scope: "sequenceClips".into(),
            speaker_labeling: "on".into(),
            language_auto_detect: false,
            default_language: "en".into(),
            whisper_model: filmcraft_speech::models::DEFAULT_MODEL.into(),
        }
    }
}

/// Settings ▸ Media Cache.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaCachePrefs {
    /// Media Cache Files location; empty = `<data dir>/Media Cache`.
    pub location: String,
    pub save_next_to_media: bool,
    /// Media Cache Database location; empty = the files location.
    pub database_location: String,
    /// `never` | `olderThan` | `exceedsSize`.
    pub management: String,
    pub older_than_days: u32,
    pub max_size_gb: u32,
    /// The render cache (fork 6.3): `smart` | `all` | `off`.
    pub render_cache: String,
    /// Size limit of the render cache in the project's preview folder (GB).
    pub render_cache_gb: u32,
    /// Local copies of clips on network shares (STORAGE-PLAN S1 item 2): `auto` | `off`
    /// (provisional default, OWNER-QUESTIONS Q49).
    pub network_copies: String,
    /// Size limit of the local copies (GB).
    pub network_copies_gb: u32,
}

impl Default for MediaCachePrefs {
    fn default() -> Self {
        Self {
            location: String::new(),
            save_next_to_media: false,
            database_location: String::new(),
            management: "never".into(),
            older_than_days: 90,
            max_size_gb: 92,
            render_cache: "smart".into(),
            render_cache_gb: crate::render_cache::DEFAULT_BUDGET_GB,
            network_copies: "auto".into(),
            network_copies_gb: crate::net_cache::DEFAULT_GB,
        }
    }
}

/// Settings ▸ Memory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MemoryPrefs {
    /// "RAM reserved for other applications" (GB).
    pub ram_reserved_gb: u32,
    /// Decoded-frame cache budget of the monitors and thumbnails (MB).
    pub frame_cache_mb: u32,
    /// Decoded source frames all clips keep for scrubbing and reverse play (MB; 0 = Auto: a
    /// fifth of the installed RAM, 1–16 GB).
    pub decode_cache_mb: u32,
    /// Decoded source frames kept in GPU memory (hardware decoding; MB; 0 = Auto: a quarter of the VRAM).
    pub vram_cache_mb: u32,
}

impl Default for MemoryPrefs {
    fn default() -> Self {
        Self { ram_reserved_gb: 4, frame_cache_mb: 768, decode_cache_mb: 0, vram_cache_mb: 0 }
    }
}

/// Settings ▸ Timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TimelinePrefs {
    pub video_transition_duration: f64,
    /// `frames` | `seconds`.
    pub video_transition_unit: String,
    pub audio_transition_duration: f64,
    pub audio_transition_unit: String,
    pub still_image_duration: f64,
    pub still_image_unit: String,
    /// "Timeline Playback Auto-Scrolling": `noScroll` | `pageScroll` | `smoothScroll`.
    pub auto_scroll: String,
    /// "Timeline Mouse Scrolling": `vertical` | `horizontal`.
    pub mouse_scrolling: String,
    /// Default Audio Tracks per media type: `useFile` | `mono` | `stereo` | `5.1` | `adaptive`.
    pub mono_media_tracks: String,
    pub stereo_media_tracks: String,
    pub surround_media_tracks: String,
    pub multichannel_mono_media_tracks: String,
    pub focus_timeline_on_edit: bool,
    pub snap_playhead: bool,
    pub return_to_beginning: bool,
    pub out_of_sync_unlinked: bool,
    pub play_after_rendering: bool,
    pub clip_mismatch_warning: bool,
    pub match_frame_sets_in: bool,
    pub restore_open_sequences: bool,
    pub add_tracks_automatically: bool,
    /// Files dropped onto the Timeline are placed directly on it.
    pub drop_import_to_timeline: bool,
}

impl Default for TimelinePrefs {
    fn default() -> Self {
        Self {
            video_transition_duration: 30.0,
            video_transition_unit: "frames".into(),
            audio_transition_duration: 1.0,
            audio_transition_unit: "seconds".into(),
            still_image_duration: 5.0,
            still_image_unit: "seconds".into(),
            auto_scroll: "pageScroll".into(),
            mouse_scrolling: "vertical".into(),
            mono_media_tracks: "useFile".into(),
            stereo_media_tracks: "useFile".into(),
            surround_media_tracks: "useFile".into(),
            multichannel_mono_media_tracks: "useFile".into(),
            focus_timeline_on_edit: false,
            snap_playhead: true,
            return_to_beginning: true,
            out_of_sync_unlinked: false,
            play_after_rendering: true,
            clip_mismatch_warning: true,
            match_frame_sets_in: false,
            restore_open_sequences: true,
            add_tracks_automatically: true,
            drop_import_to_timeline: true,
        }
    }
}

fn duration_in(value: f64, unit: &str, rate: FrameRate) -> Tick {
    let d = if unit == "frames" { rate.tick_of(value.max(1.0).round() as i64) } else { rate.snap_nearest(Tick::from_seconds_f64(value.max(0.0))) };
    d.max(rate.frame_duration())
}

impl TimelinePrefs {
    /// "Still Image Default Duration" at a sequence rate (whole frames, at least one).
    pub fn still_duration(&self, rate: FrameRate) -> Tick {
        duration_in(self.still_image_duration, &self.still_image_unit, rate)
    }
    /// "Video Transition Default Duration".
    pub fn video_transition_duration(&self, rate: FrameRate) -> Tick {
        duration_in(self.video_transition_duration, &self.video_transition_unit, rate)
    }
    /// "Audio Transition Default Duration".
    pub fn audio_transition_duration(&self, rate: FrameRate) -> Tick {
        duration_in(self.audio_transition_duration, &self.audio_transition_unit, rate)
    }
}

/// The Bins / Projects "+ modifier" rows: the platform's key names (macOS Cmd / Opt, elsewhere
/// Ctrl / Alt; the double-click reads `command` / `alt`, which are those keys).
const CMD_LABEL: &str = if cfg!(target_os = "macos") { "+ Cmd" } else { "+ Ctrl" };
const OPT_LABEL: &str = if cfg!(target_os = "macos") { "+ Opt" } else { "+ Alt" };

/// Settings ▸ Plugins.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PluginPrefs {
    pub developer_mode: bool,
}

/// Indeterminate Media Timebase choices → frame rates.
pub fn timebase_rate(v: &str) -> FrameRate {
    match v {
        "23.976" => FrameRate::FPS_23_976,
        "24" => FrameRate::FPS_24,
        "25" => FrameRate::FPS_25,
        "30" => FrameRate::FPS_30,
        "50" => FrameRate::FPS_50,
        "59.94" | "59.94df" => FrameRate::FPS_59_94,
        "60" => FrameRate::FPS_60,
        _ => FrameRate::FPS_29_97,
    }
}

// ------------------------------------------------------------------ schema

/// What kind of control a field is.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Bool,
    /// Whole number with a range and unit.
    Int {
        min: f64,
        max: f64,
        unit: &'static str,
    },
    /// Real number with a range, shown decimals and unit.
    Float {
        min: f64,
        max: f64,
        decimals: usize,
        unit: &'static str,
    },
    /// One of (value, label).
    Choice(&'static [(&'static str, &'static str)]),
    /// A number whose unit is the choice field `unit_key` (`frames` / `seconds`).
    Duration {
        unit_key: &'static str,
    },
    /// Free text.
    Text,
    /// A folder; empty = the default location.
    Path,
    /// `#rrggbb`.
    Color,
    /// A font family name.
    Font,
    /// An audio device chosen from what the host reports.
    Device(DeviceList),
    /// One of the 16 labels.
    Label,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceList {
    Hosts,
    Inputs,
    Outputs,
    /// GPUs (`playback.graphicsDevice`, L2.6).
    Gpus,
}

#[derive(Clone, Copy, Debug)]
pub struct Field {
    /// Preference key (`timeline.stillImageDuration`); the control's automation id is
    /// `settings.<key>`.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// Drawn indented under the previous field.
    pub indent: bool,
    /// Only editable while this boolean key is on.
    pub enabled_by: Option<&'static str>,
    /// Whether FilmCraft acts on the value (false = remembered for parity, no effect yet).
    pub wired: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Row {
    Field(Field),
    /// A titled group box.
    Group(&'static str, &'static [Row]),
    /// Explanatory text.
    Note(&'static str),
    /// A button running a command (`settings.<id>` automation id).
    Button {
        id: &'static str,
        label: &'static str,
        command: &'static str,
    },
    /// A page section the frontend draws itself (`labelColors`, `memoryInfo`, `mediaCacheInfo`,
    /// `outputMapping`).
    Custom(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub struct Category {
    /// Category id: first segment of its keys, `app.settings.<id>` command, `settings.category.<id>`.
    pub id: &'static str,
    pub title: &'static str,
    pub rows: &'static [Row],
}

const fn f(key: &'static str, label: &'static str, kind: Kind, wired: bool) -> Row {
    Row::Field(Field { key, label, kind, indent: false, enabled_by: None, wired })
}
const fn b(key: &'static str, label: &'static str, wired: bool) -> Row {
    f(key, label, Kind::Bool, wired)
}
const fn sub(key: &'static str, label: &'static str, kind: Kind, by: &'static str, wired: bool) -> Row {
    Row::Field(Field { key, label, kind, indent: true, enabled_by: Some(by), wired })
}
const fn int(min: f64, max: f64, unit: &'static str) -> Kind {
    Kind::Int { min, max, unit }
}
const fn float(min: f64, max: f64, decimals: usize, unit: &'static str) -> Kind {
    Kind::Float { min, max, decimals, unit }
}

const STARTUP: &[(&str, &str)] = &[("showHome", "Show Home"), ("openMostRecent", "Open Most Recent"), ("emptyProject", "Start with an Empty Project")];
const STARTUP_SHOW: &[(&str, &str)] =
    &[("workflowDefault", "Workflow default"), ("projectManager", "Project Manager"), ("lastProject", "Last project"), ("newEmpty", "New empty project")];
const MENU_BAR: &[(&str, &str)] = &[("auto", "In the desktop's global menu when it has one"), ("window", "In the window")];
const TITLE_BAR: &[(&str, &str)] = &[("filmcraft", "Revolution (like DaVinci Resolve)"), ("system", "System (desktop title bar)")];
const OPENING: &[(&str, &str)] = &[("showOpenDialog", "Show Open Dialog"), ("showHome", "Show Home")];
const BIN_OPEN: &[(&str, &str)] = &[("openInPlace", "Open in place"), ("openNewTab", "Open new tab"), ("openNewWindow", "Open in new window")];
const PROJECT_OPEN: &[(&str, &str)] = &[("openNewTab", "Open new tab"), ("openNewWindow", "Open in new window")];
const THEMES: &[(&str, &str)] = &[("darkest", "Darkest"), ("dark", "Dark"), ("light", "Light"), ("resolve", "Resolve")];
pub const APPEARANCE_MODES: &[(&str, &str)] = &[("auto", "Sync with system"), ("dark", "Dark"), ("light", "Light")];
/// Settings ▸ Appearance ▸ App icon (fork, §1a.108).
pub const APP_ICONS: &[(&str, &str)] = &[("rounded", "Rounded (macOS style)"), ("square", "Square")];
pub const MOTION: &[(&str, &str)] = &[("system", "Sync with system"), ("on", "On"), ("off", "Off (reduce motion)")];
/// The text factors "Match the desktop's text size" applies; a larger or smaller desktop factor
/// is held at the nearest end.
pub const TEXT_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.75..=2.0;
pub const UI_SCALES: &[(&str, &str)] = &[
    ("auto", "Auto (system)"),
    ("75", "75%"),
    ("100", "100%"),
    ("125", "125%"),
    ("150", "150%"),
    ("175", "175%"),
    ("200", "200%"),
    ("250", "250%"),
    ("300", "300%"),
];
pub const DARK_THEMES: &[(&str, &str)] = &[("darkest", "Darkest"), ("dark", "Dark"), ("resolve", "Resolve")];
pub const LIGHT_THEMES: &[(&str, &str)] = &[("light", "Light")];
const MIXDOWN: &[(&str, &str)] = &[("front", "Front Only"), ("frontRear", "Front + Rear"), ("frontLfe", "Front + LFE"), ("frontRearLfe", "Front + Rear + LFE")];
const AUDITION: &[(&str, &str)] = &[("scratch", "Scratch disk location for Captured Audio"), ("nextToMedia", "Next to original media files")];
const BUFFERS: &[(&str, &str)] = &[("64", "64"), ("128", "128"), ("256", "256"), ("512", "512"), ("1024", "1024"), ("2048", "2048"), ("4096", "4096")];
const RATES: &[(&str, &str)] = &[("44100", "44100"), ("48000", "48000"), ("88200", "88200"), ("96000", "96000")];
const HDR_WHITE: &[(&str, &str)] = &[("100", "100 (75% HLG, 58% PQ)"), ("203", "203 (75% HLG, 58% PQ)"), ("300", "300 (75% HLG, 58% PQ)")];
const DIRECTION: &[(&str, &str)] = &[("ltr", "Left to right"), ("rtl", "Right to left")];
const JOIN: &[(&str, &str)] = &[("miter", "Miter join"), ("round", "Round join"), ("bevel", "Bevel join")];
const CAP: &[(&str, &str)] = &[("butt", "Butt cap"), ("round", "Round cap"), ("square", "Projecting cap")];
const BG_FILL: &[(&str, &str)] = &[("allLines", "All lines"), ("perLine", "Per line")];
const TIMEBASE: &[(&str, &str)] = &[
    ("23.976", "23.976 fps"),
    ("24", "24 fps"),
    ("25", "25 fps"),
    ("29.97df", "29.97 fps Drop-Frame"),
    ("29.97", "29.97 fps Non Drop-Frame"),
    ("30", "30 fps"),
    ("50", "50 fps"),
    ("59.94df", "59.94 fps Drop-Frame"),
    ("59.94", "59.94 fps Non Drop-Frame"),
    ("60", "60 fps"),
];
const TIMECODE: &[(&str, &str)] = &[("useMediaSource", "Use Media Source"), ("generate", "Generate Timecode")];
const FRAME_COUNT: &[(&str, &str)] = &[("startAt0", "Start at 0"), ("startAt1", "Start at 1"), ("timecodeConversion", "Timecode Conversion")];
const SCALING: &[(&str, &str)] = &[("none", "None"), ("scaleToFrameSize", "Scale to frame size"), ("setToFrameSize", "Set to frame size")];
const PROXIES: &[(&str, &str)] = &[("auto", "Auto (proxies while playing, original when paused)"), ("on", "Always use proxies"), ("off", "Never use proxies")];
const ANALYSIS_CACHE: &[(&str, &str)] = &[("mediaCache", "In the Media Cache"), ("project", "In the project")];
const TRANSCRIBE_SCOPE: &[(&str, &str)] =
    &[("sequenceClips", "Auto-transcribe only clips in sequences"), ("allImported", "Auto-transcribe all imported clips")];
const SPEAKERS: &[(&str, &str)] = &[("on", "Label speakers"), ("off", "Don't label speakers")];
const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("ru", "Russian"),
    ("hi", "Hindi"),
];
const MODELS: &[(&str, &str)] =
    &[("whisper-tiny", "Whisper tiny (fastest)"), ("whisper-base", "Whisper base (balanced)"), ("whisper-small", "Whisper small (most accurate)")];
const CACHE_MGMT: &[(&str, &str)] = &[
    ("never", "Do not delete cache files automatically"),
    ("olderThan", "Automatically delete cache files older than"),
    ("exceedsSize", "Automatically delete oldest cache files when cache exceeds"),
];
const AUTO_SCROLL: &[(&str, &str)] = &[("noScroll", "No Scroll"), ("pageScroll", "Page Scroll"), ("smoothScroll", "Smooth Scroll")];
const MOUSE_SCROLL: &[(&str, &str)] = &[("vertical", "Vertical"), ("horizontal", "Horizontal")];
const HW_DECODE: &[(&str, &str)] = &[("auto", "Auto"), ("off", "Off")];
const PHONE_SHARES: &[(&str, &str)] = &[("offer", "Offer to import"), ("inbox", "Import into the Inbox bin"), ("off", "Ignore")];
const CAMERA_CARDS: &[(&str, &str)] = &[("offer", "Offer to copy"), ("off", "Ignore")];
const TRACKS: &[(&str, &str)] = &[("useFile", "Use File"), ("mono", "Mono"), ("stereo", "Stereo"), ("5.1", "5.1"), ("adaptive", "Adaptive")];
pub const DURATION_UNITS: &[(&str, &str)] = &[("frames", "Frames"), ("seconds", "Seconds")];

/// The Settings categories in Premiere's order (Adobe-cloud-only ones are left out; Plugins is a
/// stub).
pub fn categories() -> &'static [Category] {
    CATEGORIES
}

pub fn category(id: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.id == id)
}

static CATEGORIES: &[Category] = &[
    Category {
        id: "general",
        title: "General",
        rows: &[
            f(
                "general.interfaceLanguage",
                "Interface Language",
                Kind::Choice(&[
                    ("system", "System Language"),
                    ("en", "English"),
                    ("ja", "日本語"),
                    ("es", "Español"),
                    ("pt-br", "Português (Brasil)"),
                    ("uk", "Українська"),
                    ("zh-cn", "简体中文"),
                    ("de", "Deutsch"),
                    ("ru", "Русский"),
                    ("fr", "Français"),
                ]),
                true,
            ),
            // fork GRADE-PLAN G1.8: switched at once through `settings.setWorkflow`, not OK
            Row::Custom("workflow"),
            // fork: FilmCraft's own window title bar or the desktop's
            f("general.titleBar", "Window title bar", Kind::Choice(TITLE_BAR), true),
            f("general.atStartup", "At Startup", Kind::Choice(STARTUP), true),
            // fork RS2: the Project Manager home screen
            f("general.startupShow", "At startup show", Kind::Choice(STARTUP_SHOW), true),
            f("general.projectsFolder", "Projects folder", Kind::Path, true),
            f("general.whenOpeningProject", "When Opening a Project", Kind::Choice(OPENING), false),
            Row::Group(
                "Bins",
                &[
                    f("general.binsDoubleClick", "Double-click", Kind::Choice(BIN_OPEN), false),
                    f("general.binsCmdDoubleClick", CMD_LABEL, Kind::Choice(BIN_OPEN), false),
                    f("general.binsOptDoubleClick", OPT_LABEL, Kind::Choice(BIN_OPEN), false),
                ],
            ),
            Row::Group(
                "Projects",
                &[
                    f("general.projectsDoubleClick", "Double-click", Kind::Choice(PROJECT_OPEN), false),
                    f("general.projectsOptDoubleClick", OPT_LABEL, Kind::Choice(PROJECT_OPEN), false),
                ],
            ),
            b("general.showEventIndicator", "Show Event Indicator", false),
            b("general.showToolTips", "Show Tool Tips", true),
            sub("general.showRichToolTips", "Show Rich Tool Tips", Kind::Bool, "general.showToolTips", false),
            b("general.showWorkspaceResetWarning", "Show Workspace Reset Warning dialog on double click", false),
            b("general.showProjectLoadErrors", "Show Project Load Error Dialog", false),
            b("general.showCompatibilityIssues", "Show system compatibility issues at startup", false),
            b("general.showProjectBinInTab", "Show \"Project:\" and \"Bin:\" in Project panel tab", false),
            b("general.showMaskTrackerPreview", "Show Mask Tracker Preview", false),
            // fork (owner 2026-10-13): cover the desktop's panels (on by default, like Resolve), and
            // where the menu lives (the desktop's global menu or the window)
            f("general.fullScreen", "Full screen (covers the desktop's panels)", Kind::Bool, true),
            f("general.projectManagerFullScreen", "Project Manager full screen", Kind::Bool, true),
            f("general.menuBar", "Menu bar", Kind::Choice(MENU_BAR), true),
        ],
    },
    Category {
        id: "appearance",
        title: "Appearance",
        rows: &[
            // Drawn by the dialog as the Appearance Mode selector above the light and dark theme
            // cards; the fields below keep their keys, validation and `prefs.schema` entries.
            Row::Custom("appearanceModes"),
            f("appearance.appearanceMode", "Appearance Mode", Kind::Choice(APPEARANCE_MODES), true),
            f("appearance.lightTheme", "Light Theme", Kind::Choice(LIGHT_THEMES), true),
            f("appearance.darkTheme", "Dark Theme", Kind::Choice(DARK_THEMES), true),
            f("appearance.colorTheme", "Color Theme", Kind::Choice(THEMES), true),
            b("appearance.accessibleContrast", "Accessible color contrast", true),
            f("appearance.highlightColor", "Highlight Color", Kind::Color, true),
            b("appearance.followSystemAccent", "Use the system accent color", true),
            f("appearance.motion", "Animations", Kind::Choice(MOTION), true),
            f("appearance.uiScale", "UI Scale", Kind::Choice(UI_SCALES), true),
            b("appearance.matchDesktopTextSize", "Match the desktop's text size", true),
            f("appearance.appIcon", "App icon", Kind::Choice(APP_ICONS), true),
        ],
    },
    Category {
        id: "audio",
        title: "Audio",
        rows: &[
            f("audio.automatchTime", "Automatch Time", float(0.0, 30.0, 3, "seconds"), true),
            f("audio.mixdownType", "5.1 Mixdown Type", Kind::Choice(MIXDOWN), true),
            f("audio.largeVolumeAdjustment", "Large Volume Adjustment", float(0.0, 96.0, 0, "dB"), true),
            b("audio.sumToMonoInSource", "Sum multichannel outputs to mono in Source Monitor", false),
            b("audio.scrubAudio", "Play audio while scrubbing in Source and Program Monitors", false),
            b("audio.maintainPitchShuttling", "Maintain pitch while shuttling", false),
            b("audio.muteInputDuringRecording", "Mute input during timeline recording", false),
            b("audio.generateWaveformsOnImport", "Generate waveforms automatically during import", false),
            b("audio.multithreadedWaveforms", "Generate waveforms and conform audio using multithreading", false),
            b("audio.renderAudioWithVideo", "Render audio when rendering video", false),
            b("audio.autoTagAudioTypes", "Auto-tag audio types in the timeline", false),
            b("audio.alwaysOverrideAudioTags", "Always override audio tags", false),
            Row::Group(
                "Automation Keyframe Optimization",
                &[
                    b("audio.linearKeyframeThinning", "Linear keyframe thinning", true),
                    b("audio.minimumTimeIntervalThinning", "Minimum time interval thinning", true),
                    sub("audio.minimumTimeMs", "Minimum time", int(1.0, 10_000.0, "milliseconds"), "audio.minimumTimeIntervalThinning", true),
                ],
            ),
            Row::Group("Render Edit in Audition files to:", &[f("audio.editInAuditionLocation", "Location", Kind::Choice(AUDITION), false)]),
            // Fork (AUDIO-PLAN A2): the DeepFilterNet3 model behind the Voice Isolation effect.
            Row::Group("Voice isolation", &[Row::Custom("voiceIsolation")]),
        ],
    },
    Category {
        id: "audioHardware",
        title: "Audio Hardware",
        rows: &[
            f("audioHardware.deviceClass", "Device Class", Kind::Device(DeviceList::Hosts), true),
            f("audioHardware.defaultInput", "Default Input", Kind::Device(DeviceList::Inputs), false),
            f("audioHardware.defaultOutput", "Default Output", Kind::Device(DeviceList::Outputs), true),
            f("audioHardware.bufferSize", "I/O Buffer Size", Kind::Choice(BUFFERS), true),
            f("audioHardware.sampleRate", "Sample Rate", Kind::Choice(RATES), true),
            b("audioHardware.forceDocumentRate", "Attempt to force hardware to document sample rate", true),
            Row::Group(
                "Output Mapping",
                &[
                    f("audioHardware.mapLeft", "Left channel to device output", int(0.0, 63.0, ""), true),
                    f("audioHardware.mapRight", "Right channel to device output", int(0.0, 63.0, ""), true),
                    Row::Custom("outputMapping"),
                ],
            ),
        ],
    },
    Category {
        id: "autoSave",
        title: "Auto Save",
        rows: &[
            Row::Group(
                "Local Projects",
                &[
                    b("autoSave.enabled", "Automatically save projects", true),
                    sub("autoSave.intervalMinutes", "Automatically Save Every", int(1.0, 1440.0, "minute(s)"), "autoSave.enabled", true),
                    sub("autoSave.maxVersions", "Maximum Project Versions", int(1.0, 1000.0, ""), "autoSave.enabled", true),
                    sub("autoSave.saveCurrentProject", "Auto Save also saves the current project(s)", Kind::Bool, "autoSave.enabled", true),
                ],
            ),
            Row::Group(
                "Crash Recovery",
                &[
                    b("autoSave.recoveryJournal", "Keep a recovery copy of unsaved changes", true),
                    sub("autoSave.recoveryIntervalSeconds", "Update it at least every", int(1.0, 600.0, "second(s)"), "autoSave.recoveryJournal", true),
                    Row::Note(
                        "Changes are copied in the background a moment after each edit. If Revolution quits unexpectedly, they are offered the next time it starts.",
                    ),
                    Row::Custom("autoSaveStatus"),
                ],
            ),
        ],
    },
    Category {
        id: "color",
        title: "Color",
        rows: &[
            Row::Group(
                "Display Color Settings",
                &[
                    b("color.displayColorManagement", "Display Color Management", false),
                    b("color.extendedDynamicRange", "Extended Dynamic Range Monitoring", false),
                    f("color.hdrGraphicsWhite", "HDR Graphics White (Nits)", Kind::Choice(HDR_WHITE), false),
                ],
            ),
            Row::Note("For color management parameters, go to the Settings tab in the Lumetri Color panel."),
            Row::Group(
                "Grading Coach",
                &[
                    b("color.coach.hints", "Show Grading Coach hints", false),
                    b("color.coach.assistant", "Also ask the assistant (runs on this computer)", false),
                ],
            ),
            Row::Group("AI explanations", &[b("color.ai.enabled", "Explain nodes in plain words with a local AI model", true), Row::Custom("aiExplanations")]),
        ],
    },
    Category {
        id: "graphics",
        title: "Graphics",
        rows: &[
            Row::Group(
                "Text",
                &[
                    b("graphics.ligatures", "Ligatures", true),
                    b("graphics.hindiDigits", "Hindi digits", false),
                    b("graphics.smartQuotes", "Smart quotes", true),
                    f("graphics.paragraphDirection", "Default paragraph direction", Kind::Choice(DIRECTION), false),
                    f("graphics.textLineJoin", "Stroke line join", Kind::Choice(JOIN), false),
                    f("graphics.textMiterLimit", "Miter limit", float(1.0, 100.0, 1, ""), false),
                    f("graphics.textLineCap", "Stroke line cap", Kind::Choice(CAP), false),
                    f("graphics.backgroundFill", "Background fill", Kind::Choice(BG_FILL), false),
                    f("graphics.defaultFont", "Default subtitle and text font", Kind::Font, true),
                    f("graphics.missingFontReplacement", "Missing font replacement", Kind::Font, false),
                    f("graphics.emojiFont", "Default Emoji font", Kind::Font, false),
                ],
            ),
            Row::Group(
                "Shapes",
                &[
                    f("graphics.shapeLineJoin", "Stroke line join", Kind::Choice(JOIN), false),
                    f("graphics.shapeMiterLimit", "Miter limit", float(1.0, 100.0, 1, ""), false),
                    f("graphics.shapeLineCap", "Stroke line cap", Kind::Choice(CAP), false),
                ],
            ),
            Row::Group(
                "Closed Captions",
                &[
                    f("graphics.captionFont", "Default closed caption font", Kind::Font, false),
                    b("graphics.captionMonospaceOnly", "Monospace only", false),
                    b("graphics.cea708Wide", "Always import embedded CEA-708 closed captions with wide aspect.", false),
                ],
            ),
        ],
    },
    Category {
        id: "labels",
        title: "Labels",
        rows: &[
            Row::Custom("labelColors"),
            Row::Group(
                "Label Defaults",
                &[
                    f("labels.defaults.movie", "Movie (audio and video)", Kind::Label, true),
                    f("labels.defaults.video", "Video", Kind::Label, true),
                    f("labels.defaults.audio", "Audio", Kind::Label, true),
                    f("labels.defaults.still", "Still", Kind::Label, true),
                    f("labels.defaults.sequence", "Sequence", Kind::Label, true),
                    f("labels.defaults.dynamicLink", "Dynamic Link", Kind::Label, false),
                    f("labels.defaults.bin", "Bin", Kind::Label, false),
                    f("labels.defaults.captions", "Captions", Kind::Label, false),
                ],
            ),
        ],
    },
    Category {
        id: "media",
        title: "Media",
        rows: &[
            f("media.indeterminateTimebase", "Indeterminate Media Timebase", Kind::Choice(TIMEBASE), true),
            f("media.timecode", "Timecode", Kind::Choice(TIMECODE), false),
            f("media.frameCount", "Frame Count", Kind::Choice(FRAME_COUNT), false),
            f("media.defaultMediaScaling", "Default Media Scaling", Kind::Choice(SCALING), true),
            b("media.writeXmpId", "Write XMP ID to files on import", false),
            b("media.validateContentCredentials", "Validate Content Credentials on import", false),
            b("media.writeClipMarkersToXmp", "Write clip markers to XMP", false),
            b("media.enableXmpLinking", "Enable clip and XMP metadata linking", false),
            b("media.includeCaptionsOnImport", "Include captions on import", false),
            f("media.proxies", "Proxies", Kind::Choice(PROXIES), true),
            b("media.autoProxies", "Create proxies automatically for 4K and larger long-GOP clips", true),
            b("media.allowDuplicateMedia", "Allow duplicate media during project import", false),
            b("media.createFolderForImportedProjects", "Create folder for imported projects", false),
            b("media.autoHideDependentClips", "Automatically Hide Dependent Clips", false),
            b("media.importImageSequences", "Import numbered stills as image sequences", false),
            Row::Group(
                "Growing Files",
                &[
                    b("media.refreshGrowingFiles", "Automatically refresh growing files", true),
                    b("media.resumeGrowingPlayback", "Automatically resume playback for growing files in Source Monitor", false),
                    sub("media.growingRefreshSeconds", "Refresh growing Files Every", int(1.0, 3600.0, "seconds"), "media.refreshGrowingFiles", true),
                ],
            ),
            b("media.hardwareDecoding", "Enable hardware accelerated decoding (requires restart)", false),
            b("media.proresHardwareEncoding", "Enable ProRes hardware accelerated encoding, if available", false),
            f("media.phoneShares", "Files shared from a phone (KDE Connect)", Kind::Choice(PHONE_SHARES), true),
            f("media.cameraCards", "Camera cards", Kind::Choice(CAMERA_CARDS), true),
            f("media.cardDestination", "Copy camera cards to", Kind::Path, true),
        ],
    },
    Category {
        id: "mediaAnalysis",
        title: "Media Analysis & Transcription",
        rows: &[
            f("mediaAnalysis.cacheResults", "Cache analysis results for re-use", Kind::Choice(ANALYSIS_CACHE), false),
            Row::Note(
                "Media analysis and transcriptions will be cached in the Media Cache folder. Reanalysis won't be required if the media is imported into another project on this computer.",
            ),
            Row::Group("Media Analysis", &[b("mediaAnalysis.analyzeImportedMedia", "Analyze all imported media to search for visuals or audio", false)]),
            Row::Group(
                "Transcription",
                &[
                    b("mediaAnalysis.autoTranscribe", "Automatically transcribe clips", true),
                    sub("mediaAnalysis.autoTranscribeScope", "Transcription preferences", Kind::Choice(TRANSCRIBE_SCOPE), "mediaAnalysis.autoTranscribe", true),
                    f("mediaAnalysis.speakerLabeling", "Speaker Labeling", Kind::Choice(SPEAKERS), true),
                    b("mediaAnalysis.languageAutoDetect", "Enable language auto-detection", true),
                    f("mediaAnalysis.defaultLanguage", "Default language", Kind::Choice(LANGUAGES), true),
                    f("mediaAnalysis.whisperModel", "Speech model", Kind::Choice(MODELS), true),
                ],
            ),
        ],
    },
    Category {
        id: "mediaCache",
        title: "Media Cache",
        rows: &[
            Row::Group(
                "Media Cache Files",
                &[
                    f("mediaCache.location", "Location", Kind::Path, true),
                    b("mediaCache.saveNextToMedia", "Save .cfa and .pek media cache files next to original media files when possible", false),
                    Row::Button { id: "mediaCache.clean", label: "Delete…", command: "mediaCache.clean" },
                    Row::Custom("mediaCacheInfo"),
                ],
            ),
            Row::Group("Media Cache Database", &[f("mediaCache.databaseLocation", "Location", Kind::Path, false)]),
            Row::Group(
                "Media Cache Management",
                &[
                    f("mediaCache.management", "Policy", Kind::Choice(CACHE_MGMT), true),
                    f("mediaCache.olderThanDays", "Delete files older than", int(1.0, 3650.0, "days"), true),
                    f("mediaCache.maxSizeGb", "Delete oldest files when the cache exceeds", int(1.0, 100_000.0, "GB"), true),
                ],
            ),
            Row::Group(
                "Render Cache",
                &[
                    f("mediaCache.renderCache", "Render cache", Kind::Choice(crate::render_cache::MODES), true),
                    f("mediaCache.renderCacheGb", "Render cache size limit", int(1.0, 100_000.0, "GB"), true),
                    Row::Custom("renderCacheInfo"),
                ],
            ),
            Row::Group(
                "Network Copies",
                &[
                    f("mediaCache.networkCopies", "Local copies of clips on network shares", Kind::Choice(crate::net_cache::MODES), true),
                    f("mediaCache.networkCopiesGb", "Local copies size limit", int(1.0, 100_000.0, "GB"), true),
                ],
            ),
            Row::Note(
                "The media cache holds render previews of unsaved projects and other files Revolution can recreate. Deleting them is always safe; Revolution rebuilds them as needed.",
            ),
        ],
    },
    Category {
        id: "memory",
        title: "Memory",
        rows: &[
            Row::Custom("memoryInfo"),
            f("memory.ramReservedGb", "RAM reserved for other applications", int(1.0, 1024.0, "GB"), false),
            f("memory.frameCacheMb", "Frame cache for monitors and thumbnails", int(64.0, 65_536.0, "MB"), true),
            f("memory.decodeCacheMb", "Decoded source frames (0 = Auto)", int(0.0, 262_144.0, "MB"), true),
            f("memory.vramCacheMb", "Decoded frames in GPU memory (0 = Auto)", int(0.0, 65_536.0, "MB"), true),
        ],
    },
    Category {
        id: "playback",
        title: "Playback",
        rows: &[
            f("playback.prerollSeconds", "Preroll", float(0.0, 60.0, 0, "seconds"), true),
            f("playback.postrollSeconds", "Postroll", float(0.0, 60.0, 0, "seconds"), true),
            f("playback.stepManyFrames", "Step forward/back many", int(1.0, 1000.0, "frames"), true),
            b("playback.pauseEncoderQueue", "Pause Media Encoder queue during playback", false),
            b("playback.enableTransmit", "Enable Mercury Transmit", false),
            b("playback.disableVideoInBackground", "Disable video output when in the background", false),
            b("playback.draftDecode", "Draft decoding at reduced playback resolution (H.264: faster, some frames less filtered)", true),
            f("playback.hardwareDecoding", "Hardware decoding", Kind::Choice(HW_DECODE), true),
            b("playback.desktopMediaControls", "Media keys and the desktop's media controls", true),
            b("playback.desktopGlobalShortcuts", "Global shortcuts: Play / Pause and Add Marker while another window has focus", true),
            f("playback.graphicsDevice", "Graphics device (takes effect at the next start)", Kind::Device(DeviceList::Gpus), true),
            b("playback.batteryPreview", "Lower playback resolution on battery", false),
            Row::Note(
                "Hardware decoding: Auto uses the system's video decoder (VideoToolbox on macOS, Media Foundation on Windows, VA-API or NVDEC for H.264 and HEVC on Linux) for the streams it supports, and Revolution's own decoder for everything else or if the hardware fails. Media that is already open keeps its decoder until it is reopened.",
            ),
        ],
    },
    Category {
        id: "plugins",
        title: "Plugins",
        rows: &[
            Row::Group("Plugins", &[b("plugins.developerMode", "Enable developer mode", false)]),
            Row::Note("No plugins are installed. Changes will take effect the next time you start Revolution."),
        ],
    },
    Category {
        id: "timeline",
        title: "Timeline",
        rows: &[
            f("timeline.videoTransitionDuration", "Video Transition Default Duration", Kind::Duration { unit_key: "timeline.videoTransitionUnit" }, true),
            f("timeline.audioTransitionDuration", "Audio Transition Default Duration", Kind::Duration { unit_key: "timeline.audioTransitionUnit" }, true),
            f("timeline.stillImageDuration", "Still Image Default Duration", Kind::Duration { unit_key: "timeline.stillImageUnit" }, true),
            f("timeline.autoScroll", "Timeline Playback Auto-Scrolling", Kind::Choice(AUTO_SCROLL), true),
            f("timeline.mouseScrolling", "Timeline Mouse Scrolling", Kind::Choice(MOUSE_SCROLL), true),
            Row::Group(
                "Default Audio Tracks",
                &[
                    f("timeline.monoMediaTracks", "Mono Media", Kind::Choice(TRACKS), false),
                    f("timeline.stereoMediaTracks", "Stereo Media", Kind::Choice(TRACKS), false),
                    f("timeline.surroundMediaTracks", "5.1 Media", Kind::Choice(TRACKS), false),
                    f("timeline.multichannelMonoMediaTracks", "Multichannel Mono Media", Kind::Choice(TRACKS), false),
                ],
            ),
            b("timeline.focusTimelineOnEdit", "Set focus on the Timeline when performing Insert/Overwrite edits", false),
            b("timeline.snapPlayhead", "Snap playhead in Timeline when Snap is enabled", true),
            b("timeline.returnToBeginning", "At playback end, return to beginning when restarting playback", true),
            b("timeline.outOfSyncUnlinked", "Display out of sync indicators for unlinked clips", false),
            b("timeline.playAfterRendering", "Play after rendering previews", true),
            b("timeline.clipMismatchWarning", "Show Clip Mismatch Warning dialog", false),
            b("timeline.matchFrameSetsIn", "Match frame sets in point", false),
            b("timeline.restoreOpenSequences", "Restore open sequences when opening projects", false),
            b("timeline.addTracksAutomatically", "Add tracks automatically when editing source clips onto the Timeline", false),
            b("timeline.dropImportToTimeline", "Place files dropped onto the Timeline directly on the Timeline", true),
        ],
    },
    Category {
        id: "trim",
        title: "Trim",
        rows: &[
            f("trim.largeTrimOffset", "Large Trim Offset", int(1.0, 1000.0, "frames"), true),
            f("trim.largeTrimOffsetAudio", "", int(1.0, 100_000.0, "Audio Time Units"), false),
            b("trim.selectionToolRollRipple", "Allow Selection tool to choose Roll and Ripple trims without modifier key", true),
            b("trim.toolChangesTrimType", "Allow current tool to change trim type of previously selected edit point", false),
            b("trim.shiftOverlappingClips", "Shift clips that overlap trim point during ripple trimming", false),
            sub("trim.rippleAddsEdits", "Ripple trim adds edits to keep both sides of trim in sync", Kind::Bool, "trim.shiftOverlappingClips", false),
            b("trim.playheadDeterminesLoop", "Playhead position determines trim monitor loop playback", true),
            b("trim.dynamicRippleUpdates", "Update timeline dynamically during ripple edits.", false),
        ],
    },
    // STORAGE-PLAN S2: servers used without mounting them; secrets in the system keyring
    Category {
        id: "remote",
        title: "Remote Locations",
        rows: &[
            Row::Note(
                "Servers Revolution connects to by itself, without mounting them: WebDAV (Nextcloud, ownCloud, Synology), SFTP (any SSH server) and S3 (MinIO, Garage, Amazon S3). Passwords and keys are kept in the system keyring, never in projects or logs.",
            ),
            Row::Custom("remoteLocations"),
            b("remote.secretsInFile", "Keep passwords in a file when no system keyring works (not encrypted)", false),
            // S2b (ADR 0008): one port to open in a strict firewall instead of a new one each time
            f("remote.nearbyPort", "Port for receiving from a nearby computer (0 = the standard port, 47821)", int(0.0, 65535.0, ""), true),
        ],
    },
    // fork UPD (FORK-PLAN §1a.77): builds without an update channel show "doesn't check"
    Category {
        id: "update",
        title: "Updates",
        rows: &[
            b("update.check", "Check for updates every day", true),
            Row::Note(
                "What the check sends: one request to GitHub (api.github.com) asking for the newest release. GitHub sees your IP address; nothing else is sent: no usage data, no identifiers.",
            ),
            Row::Custom("updateInfo"),
        ],
    },
];

/// Every field of every category (groups flattened), in dialog order.
pub fn fields() -> Vec<(&'static str, Field)> {
    fn walk(cat: &'static str, rows: &'static [Row], out: &mut Vec<(&'static str, Field)>) {
        for r in rows {
            match r {
                Row::Field(f) => out.push((cat, *f)),
                Row::Group(_, inner) => walk(cat, inner, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for c in CATEGORIES {
        walk(c.id, c.rows, &mut out);
    }
    out
}

pub fn field(key: &str) -> Option<Field> {
    fields().into_iter().map(|(_, f)| f).find(|f| f.key == key)
}

/// Check a value for `key` against the schema (choices, colours). Ranges are clamped later.
pub fn validate(key: &str, v: &Value) -> std::result::Result<(), String> {
    let kind = match field(key) {
        Some(f) => f.kind,
        None if is_unit_key(key) => Kind::Choice(DURATION_UNITS),
        None if key.starts_with("labels.colors.") && key.ends_with(".color") => Kind::Color,
        None => return Ok(()),
    };
    match kind {
        Kind::Choice(opts) => {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return Err(format!("`{key}` must be one of {}", opts.iter().map(|o| o.0).collect::<Vec<_>>().join(", "))),
            };
            if opts.iter().any(|o| o.0 == s) {
                Ok(())
            } else {
                Err(format!("`{key}` must be one of {}", opts.iter().map(|o| o.0).collect::<Vec<_>>().join(", ")))
            }
        }
        Kind::Color if v.as_str().and_then(parse_hex).is_none() => Err(format!("`{key}` must be a colour like \"#2f6bdf\"")),
        _ => Ok(()),
    }
}

/// Whether `key` is the unit (`frames` / `seconds`) of a duration field.
pub fn is_unit_key(key: &str) -> bool {
    fields().iter().any(|(_, f)| matches!(f.kind, Kind::Duration { unit_key } if unit_key == key))
}

/// Repair a preferences value in place against the schema: unknown choices and bad colours go back
/// to their defaults, numbers are clamped to their ranges.
pub fn sanitize(v: &mut Value, defaults: &Value) {
    // Workflow (top level, outside the dialog's schema): garbage falls back to the default instead
    // of failing the whole file.
    if let Some(m) = v.as_object_mut() {
        if m.get("workflow").is_some_and(|w| !matches!(w.as_str(), Some("premiere" | "resolve"))) {
            m.remove("workflow");
        }
        if m.get("workflowReturnPreset").is_some_and(|w| !w.is_string()) {
            m.remove("workflowReturnPreset");
        }
        for key in ["workflowReturnTheme", "workflowAutoTheme"] {
            if m.get(key).is_some_and(|w| !w.is_string()) {
                m.remove(key);
            }
        }
    }
    for (_, f) in fields() {
        if let Kind::Duration { unit_key } = f.kind {
            let ptr = format!("/{}", unit_key.replace('.', "/"));
            let def = defaults.pointer(&ptr).cloned().unwrap_or(json!("seconds"));
            if let Some(slot) = v.pointer_mut(&ptr)
                && validate(unit_key, slot).is_err()
            {
                *slot = def;
            }
        }
    }
    for (_, f) in fields() {
        let path: Vec<&str> = f.key.split('.').collect();
        let Some(slot) = path.iter().try_fold(&mut *v, |v, k| v.get_mut(*k)) else { continue };
        let def = path.iter().try_fold(defaults, |v, k| v.get(*k)).cloned().unwrap_or(Value::Null);
        match f.kind {
            Kind::Choice(_) | Kind::Color if validate(f.key, slot).is_err() => {
                *slot = def;
            }
            // a hand-edited file with a number where a name goes: that key's default, not the
            // whole file's (serde would refuse the file)
            Kind::Device(_) | Kind::Text | Kind::Path if !slot.is_string() && (def.is_string() || (def.is_null() && !slot.is_null())) => {
                *slot = def;
            }
            Kind::Int { min, max, .. } => match slot.as_f64().filter(|x| x.is_finite()) {
                Some(x) => *slot = json!(x.round().clamp(min, max) as u64),
                None => *slot = def,
            },
            Kind::Float { min, max, .. } => match slot.as_f64().filter(|x| x.is_finite()) {
                Some(x) => *slot = json!(x.clamp(min, max)),
                None => *slot = def,
            },
            Kind::Duration { unit_key } => {
                let ptr = format!("/{}", unit_key.replace('.', "/"));
                let unit = v.pointer(&ptr).or(defaults.pointer(&ptr)).and_then(Value::as_str).unwrap_or("seconds").to_string();
                let Some(slot) = path.iter().try_fold(&mut *v, |v, k| v.get_mut(*k)) else { continue };
                let max = if unit == "frames" { 100_000.0 } else { 3600.0 };
                let min = if unit == "frames" { 1.0 } else { 0.01 };
                match slot.as_f64().filter(|x| x.is_finite()) {
                    Some(x) if unit == "frames" => *slot = json!(x.round().clamp(min, max)),
                    Some(x) => *slot = json!(x.clamp(min, max)),
                    None => *slot = def,
                }
            }
            _ => {}
        }
    }
}

/// Upgrade an older preferences file (`version` missing = v1) to [`PREFS_VERSION`].
pub fn migrate(v: &mut Value) {
    let Some(m) = v.as_object_mut() else { return };
    let ver = m.get("version").and_then(Value::as_u64).unwrap_or(1);
    if ver < 3 {
        // v3 (fork RS1) only renumbers; the fork build's one-time switch to Resolve is decided by
        // `fork_resolve::after_load` from the version the file had.
        m.insert("version".into(), json!(3));
    }
    if ver < 2 {
        // v1 kept the Trim Monitor's loop choice and pre/postroll; v2 adds every other category
        // (filled in from defaults by serde). The v1 "Auto Save" dialog wrote integer seconds for
        // pre/postroll; v2 stores them as reals, which serde reads either way.
        m.insert("version".into(), json!(2));
    }
    // Files from before fork 6.2 had an "Enable proxies" switch: on stays on, off becomes Auto
    // (proxies were off only because there were none to use)
    if let Some(media) = m.get_mut("media").and_then(Value::as_object_mut)
        && let Some(old) = media.remove("enableProxies")
        && !media.contains_key("proxies")
    {
        media.insert("proxies".into(), json!(if old.as_bool() == Some(true) { "on" } else { "auto" }));
    }
    // Files from before appearance modes had one Color Theme: keep showing it, as a fixed Dark or
    // Light mode, instead of switching established users to another theme or to Auto.
    if let Some(a) = m.get_mut("appearance").and_then(Value::as_object_mut)
        && !a.contains_key("appearanceMode")
        && let Some(theme) = a.get("colorTheme").and_then(Value::as_str).map(str::to_string)
    {
        let mut p = AppearancePrefs::default();
        p.select_theme(&theme);
        a.insert("appearanceMode".into(), json!(p.appearance_mode));
        a.insert("darkTheme".into(), json!(p.dark_theme));
        a.insert("lightTheme".into(), json!(p.light_theme));
    }
}

impl crate::autosave::Preferences {
    /// Settings ▸ Playback ▸ Hardware decoding as the process-wide switch the hardware decoder
    /// factories consult (new decoders only).
    pub fn apply_hardware_decoding(&self) {
        filmcraft_codecs::hw::set_hardware_decoding(self.playback.hardware_decoding != "off");
        self.apply_decode_cache();
    }

    /// Settings ▸ Memory ▸ Decoded source frames: the budget all clips' decoded frames share (Auto
    /// from the installed RAM; the live-decoder cap always follows the RAM).
    pub fn apply_decode_cache(&self) {
        let (auto_budget, decoders) = filmcraft_codecs::auto_limits(filmcraft_codecs::installed_ram());
        let budget = match self.memory.decode_cache_mb {
            0 => auto_budget,
            mb => (mb as usize).saturating_mul(1 << 20),
        };
        filmcraft_codecs::configure_decode_cache(budget, decoders);
        filmcraft_codecs::configure_vram(match self.memory.vram_cache_mb {
            0 => filmcraft_codecs::auto_vram_budget_for(filmcraft_codecs::decode_memory()),
            mb => (mb as usize).saturating_mul(1 << 20),
        });
    }

    /// Values of one category as a JSON object.
    pub fn category_value(&self, id: &str) -> Option<Value> {
        self.to_value().get(id).cloned()
    }
    /// Reset one category to its defaults (`recentProjects` survives a General reset).
    pub fn reset_category(&mut self, id: &str) -> std::result::Result<(), String> {
        if category(id).is_none() {
            return Err(format!("unknown settings category `{id}`"));
        }
        let mut v = self.to_value();
        let d = Self::default().to_value();
        let keep = self.general.recent_projects.clone();
        v[id] = d[id].clone();
        *self = serde_json::from_value(v).map_err(|e| e.to_string())?;
        self.general.recent_projects = keep;
        Ok(())
    }
    /// Remember a project as the most recent one.
    pub fn note_recent(&mut self, path: &str) {
        let r = &mut self.general.recent_projects;
        r.retain(|p| p != path);
        r.insert(0, path.to_string());
        r.truncate(10);
    }
}

// ------------------------------------------------------------------ helpers used by the engine

/// Typographer's quotes: `"a 'b'"` → `“a ‘b’”`, apostrophes inside words → `’`.
pub fn smart_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev: Option<char> = None;
    for c in s.chars() {
        let opening = prev.is_none_or(|p| p.is_whitespace() || "([{-–—“‘".contains(p));
        out.push(match c {
            '"' if opening => '“',
            '"' => '”',
            '\'' if opening => '‘',
            '\'' => '’',
            c => c,
        });
        prev = Some(c);
    }
    out
}

/// Interleave a stereo mix into a device buffer of `ch` channels following Output Mapping: left
/// to device channel `map[0]`, right to `map[1]` (others silent). A mono device gets the sum.
/// Out-of-range mappings fall back to channels 0/1.
pub fn map_output(left: &[f32], right: &[f32], out: &mut [f32], ch: usize, map: [u32; 2]) {
    let ch = ch.max(1);
    let n = (out.len() / ch).min(left.len()).min(right.len());
    out.iter_mut().for_each(|s| *s = 0.0);
    if ch == 1 {
        for i in 0..n {
            out[i] = (0.5 * (left[i] + right[i])).clamp(-1.0, 1.0);
        }
        return;
    }
    let pick = |m: u32, d: usize| if (m as usize) < ch { m as usize } else { d };
    let (l, r) = (pick(map[0], 0), pick(map[1], 1));
    for i in 0..n {
        out[i * ch + l] += left[i];
        out[i * ch + r] += right[i];
        out[i * ch + l] = out[i * ch + l].clamp(-1.0, 1.0);
        out[i * ch + r] = out[i * ch + r].clamp(-1.0, 1.0);
    }
}

/// Settings ▸ Media ▸ Default Media Scaling for a clip placed in a `frame`-sized sequence. `src`
/// is the clip's size at its display aspect in sequence pixels
/// ([`filmcraft_project::conformed_size`]): `scaleToFrameSize` turns on Scale to Frame Size
/// (rasterised at frame size), `setToFrameSize` sets Motion ▸ Scale so the picture fits the frame.
pub fn apply_media_scaling(ti: &mut filmcraft_project::TrackItem, scaling: &str, frame: (u32, u32), src: (f64, f64)) {
    if !(src.0 >= 1e-9 && src.1 >= 1e-9 && src.0.is_finite() && src.1.is_finite()) {
        return;
    }
    match scaling {
        "scaleToFrameSize" => ti.scale_to_frame = true,
        "setToFrameSize" => {
            let fit = (frame.0 as f64 / src.0).min(frame.1 as f64 / src.1);
            if let Some(m) = ti.effects.iter_mut().find(|e| e.effect == "motion") {
                m.params.insert("scale".into(), filmcraft_project::Param::new(filmcraft_project::ParamValue::Float((fit * 1000.0).round() / 10.0)));
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ media cache

/// The media cache folder: the configured location, else `Media Cache` in the cache directory
/// (`$XDG_CACHE_HOME/filmcraft` for a default Linux install, else `<data dir>`; [`crate::dirs`]).
pub fn media_cache_dir(prefs: &MediaCachePrefs, data_dir: Option<&Path>) -> Option<PathBuf> {
    if !prefs.location.trim().is_empty() {
        return Some(PathBuf::from(prefs.location.trim()));
    }
    data_dir.map(|d| crate::dirs::AppDirs::for_data_dir(d).media_cache())
}

/// Where unsaved projects keep their render previews inside the media cache.
pub fn previews_root(cache: &Path) -> PathBuf {
    cache.join("Previews")
}

/// Files under `dir` (recursively): (path, bytes, modified).
fn cache_files(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    // the local copies of share clips keep their own size limit and index (S1 item 2)
    let copies = dir.join(crate::net_cache::DIR_NAME);
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() && e.path() == copies {
                continue;
            }
            if md.is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), md.len(), md.modified().unwrap_or(std::time::UNIX_EPOCH)));
            }
        }
    }
    out
}

/// Size and file count of the media cache.
pub fn cache_usage(dir: &Path) -> (u64, usize) {
    let f = cache_files(dir);
    (f.iter().map(|x| x.1).sum(), f.len())
}

/// Remove files under `dir`, skipping anything inside `keep`; returns (files, bytes) removed.
fn remove_files(files: &[(PathBuf, u64, std::time::SystemTime)], keep: Option<&Path>) -> (usize, u64) {
    let mut n = (0usize, 0u64);
    for (p, len, _) in files {
        if keep.is_some_and(|k| p.starts_with(k)) {
            continue;
        }
        if std::fs::remove_file(p).is_ok() {
            n.0 += 1;
            n.1 += len;
        }
    }
    n
}

fn prune_empty_dirs(dir: &Path, keep: Option<&Path>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && keep.is_none_or(|k| !k.starts_with(&p)) {
            prune_empty_dirs(&p, keep);
            let _ = std::fs::remove_dir(&p);
        } else if p.is_dir() {
            prune_empty_dirs(&p, keep);
        }
    }
}

/// Media Cache Management: apply the automatic deletion policy to `dir` at `now`, never touching
/// `keep` (the open project's files). Returns (files, bytes) removed.
pub fn enforce_policy(dir: &Path, prefs: &MediaCachePrefs, keep: Option<&Path>, now: std::time::SystemTime) -> (usize, u64) {
    let mut files = cache_files(dir);
    let removed = match prefs.management.as_str() {
        "olderThan" => {
            let max_age = std::time::Duration::from_secs(prefs.older_than_days as u64 * 86_400);
            let old: Vec<_> = files.into_iter().filter(|f| now.duration_since(f.2).is_ok_and(|a| a > max_age)).collect();
            remove_files(&old, keep)
        }
        "exceedsSize" => {
            let limit = prefs.max_size_gb as u64 * 1_000_000_000;
            let mut total: u64 = files.iter().map(|f| f.1).sum();
            files.sort_by_key(|f| f.2);
            let mut doomed = Vec::new();
            for f in files {
                if total <= limit {
                    break;
                }
                if keep.is_some_and(|k| f.0.starts_with(k)) {
                    continue;
                }
                total -= f.1;
                doomed.push(f);
            }
            remove_files(&doomed, keep)
        }
        _ => (0, 0),
    };
    prune_empty_dirs(dir, keep);
    removed
}

/// Delete everything in the media cache except `keep` ("Delete…" ▸ unused files).
pub fn clean(dir: &Path, keep: Option<&Path>) -> (usize, u64) {
    let files = cache_files(dir);
    let r = remove_files(&files, keep);
    prune_empty_dirs(dir, keep);
    r
}

impl Session {
    /// The media cache folder (None without a data directory, e.g. tests and the web).
    pub fn media_cache_dir(&self) -> Option<PathBuf> {
        if cfg!(target_arch = "wasm32") {
            return None;
        }
        media_cache_dir(&self.prefs.media_cache, self.prefs_path.as_deref().and_then(Path::parent))
    }

    /// Point unsaved projects' render previews into the media cache and apply the automatic
    /// deletion policy (on startup and when the Media Cache settings change).
    pub fn apply_media_cache(&mut self) {
        let Some(dir) = self.media_cache_dir() else { return };
        let root = previews_root(&dir);
        if self.previews.temp_root() != Some(root.clone()) {
            self.previews.set_temp_root(Some(root));
            if self.path.is_none() && self.previews.count() == 0 {
                self.previews.reset_temp();
            }
        }
        let keep = self.previews.dir();
        enforce_policy(&dir, &self.prefs.media_cache, keep.as_deref(), std::time::SystemTime::now());
        // local copies of share clips: on in `auto`, read by the pool once complete
        let copies = (self.prefs.media_cache.network_copies == "auto").then(|| dir.join(crate::net_cache::DIR_NAME));
        let current = self.media.net_copies().map(|c| c.lock().unwrap_or_else(std::sync::PoisonError::into_inner).dir().to_path_buf());
        if copies != current {
            // a copy running into the old cache stops (it would write the index the new one reads)
            if let Some(job) = &self.net_copy {
                job.stop();
            }
            self.media.set_net_copies(copies.map(|d| std::sync::Arc::new(std::sync::Mutex::new(crate::net_cache::NetCache::open(&d)))));
        }
    }

    /// Note a project file as the most recent one (General ▸ At Startup ▸ Open Most Recent).
    pub fn note_recent_project(&mut self) {
        let Some(p) = self.path.clone() else { return };
        if self.prefs.general.recent_projects.first() == Some(&p) {
            return;
        }
        let mut next = self.prefs.clone();
        next.note_recent(&p);
        let _ = self.set_prefs(next);
    }
}

// ------------------------------------------------------------------ commands

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
}

fn kind_json(k: Kind) -> Value {
    match k {
        Kind::Bool => json!({"type": "bool"}),
        Kind::Int { min, max, unit } => json!({"type": "int", "min": min, "max": max, "unit": unit}),
        Kind::Float { min, max, decimals, unit } => json!({"type": "float", "min": min, "max": max, "decimals": decimals, "unit": unit}),
        Kind::Choice(o) => json!({"type": "choice", "choices": o.iter().map(|(v, l)| json!({"value": v, "label": l})).collect::<Vec<_>>()}),
        Kind::Duration { unit_key } => json!({"type": "duration", "unitKey": unit_key}),
        Kind::Text => json!({"type": "text"}),
        Kind::Path => json!({"type": "path"}),
        Kind::Color => json!({"type": "color"}),
        Kind::Font => json!({"type": "font"}),
        Kind::Device(d) => json!({"type": "device", "list": format!("{d:?}").to_ascii_lowercase()}),
        Kind::Label => json!({"type": "label", "choices": Label::ALL.iter().map(|l| l.name()).collect::<Vec<_>>()}),
    }
}

fn schema(s: &mut Session, p: &Value) -> Result<Value> {
    let want = str_p(p, "category");
    let cats: Vec<Value> = CATEGORIES
        .iter()
        .filter(|c| want.is_none_or(|w| w == c.id))
        .map(|c| {
            let fields: Vec<Value> = fields()
                .into_iter()
                .filter(|(cat, _)| *cat == c.id)
                .map(|(_, f)| {
                    json!({"key": f.key, "label": f.label, "kind": kind_json(f.kind), "wired": f.wired, "enabledBy": f.enabled_by, "value": s.prefs.get(f.key)})
                })
                .collect();
            json!({"id": c.id, "title": c.title, "command": format!("app.settings.{}", c.id), "fields": fields})
        })
        .collect();
    if let Some(w) = want
        && cats.is_empty()
    {
        return Err(bad("prefs.schema", format!("unknown category `{w}`")));
    }
    Ok(json!({"categories": cats}))
}

/// `app.paths`: the data / config / cache / state folders and where logs and the media cache go
/// (`null` folders in a session without a data directory: tests, the web, the CLI).
fn app_paths(s: &mut Session, _: &Value) -> Result<Value> {
    let dirs = s.prefs_path.as_deref().and_then(Path::parent).filter(|_| !cfg!(target_arch = "wasm32")).map(crate::dirs::AppDirs::for_data_dir);
    let mut v = dirs.map(|d| d.to_json()).unwrap_or_else(|| json!({"data": null, "config": null, "cache": null, "state": null, "logs": null}));
    if let Some(o) = v.as_object_mut() {
        o.insert("mediaCache".into(), json!(s.media_cache_dir().map(|d| d.to_string_lossy().into_owned())));
        o.insert("xdg".into(), json!(crate::dirs::xdg_platform()));
    }
    Ok(v)
}

fn cache_info(s: &mut Session, _: &Value) -> Result<Value> {
    let dir = s.media_cache_dir();
    let (bytes, files) = dir.as_deref().map(cache_usage).unwrap_or((0, 0));
    Ok(json!({"location": dir.map(|d| d.to_string_lossy().into_owned()), "bytes": bytes, "files": files, "policy": s.prefs.media_cache.management}))
}

fn cache_clean(s: &mut Session, p: &Value) -> Result<Value> {
    let dir = s.media_cache_dir().ok_or_else(|| EngineError::Other("there is no media cache folder in this session".into()))?;
    // the open project's previews stay unless `all` is set
    let keep = if bool_p(p, "all") == Some(true) { None } else { s.previews.dir() };
    let (files, bytes) = clean(&dir, keep.as_deref());
    if keep.is_none() {
        s.previews.set_dir(s.previews.dir());
    }
    Ok(json!({"files": files, "bytes": bytes, "location": dir.to_string_lossy()}))
}

fn workflow_json(s: &Session) -> Value {
    json!({"workflow": s.prefs.workflow, "keyboardPreset": s.shortcuts.preset, "returnPreset": s.prefs.workflow_return_preset})
}

/// `Preferences::workflow_auto_theme` once a hand-picked theme was kept across a switch: it is no
/// theme name, so the theme in use never counts as set by a switch again.
const HAND_PICKED_THEME: &str = "hand-picked";

impl Session {
    /// Settings ▸ Workflow: switch the keyboard between Premiere conventions and Resolve's.
    ///
    /// To Resolve: remembers the active keyboard preset (an edited set is first saved under a free
    /// name "<name> (before Resolve)" so no edit is lost; if that save fails nothing changes), then
    /// loads "DaVinci Resolve Compatible". Back to Premiere: if the user loaded another preset
    /// while in Resolve, that one stays; otherwise an edited Resolve set is saved as "DaVinci
    /// Resolve Compatible (edited)" (free name; failure refuses the switch) and the remembered
    /// preset is restored (FilmCraft Default when there is none or it was deleted). Saved presets
    /// are never overwritten. The dark theme swaps only when it is the one the last switch set
    /// itself (Resolve theme in Resolve, the remembered one back in Premiere); a theme picked by
    /// hand follows you in both directions. The choice persists in the preferences.
    pub fn set_workflow(&mut self, w: Workflow) -> Result<()> {
        use crate::shortcuts::{DEFAULT_PRESET, RESOLVE_PRESET};
        const CMD: &str = "settings.setWorkflow";
        if self.prefs.workflow == w {
            return Ok(());
        }
        let mut next = self.prefs.clone();
        match w {
            Workflow::Resolve => {
                if self.shortcuts.preset != RESOLVE_PRESET {
                    let mut back = self.shortcuts.preset.clone();
                    if self.shortcuts.modified {
                        let name = self.shortcuts.free_preset_name(&format!("{back} (before Resolve)"));
                        self.shortcuts.save_preset(&name).map_err(|e| bad(CMD, format!("could not keep your edited shortcuts as `{name}`: {e}")))?;
                        back = name;
                    }
                    next.workflow_return_preset = back;
                }
                self.shortcuts.load_preset(RESOLVE_PRESET).map_err(|e| bad(CMD, e))?;
                // a dark theme the user picked by hand (different from the one the last switch set
                // itself) follows them; otherwise Premiere's is remembered for the way back and
                // the Resolve theme shows (`workflow_auto_theme` = what the switch set last)
                let cur = next.appearance.dark_theme.clone();
                if !next.workflow_auto_theme.is_empty() && cur != next.workflow_auto_theme {
                    next.workflow_return_theme.clear();
                    next.workflow_auto_theme = HAND_PICKED_THEME.to_string();
                } else {
                    next.workflow_return_theme = cur;
                    next.workflow_auto_theme = RESOLVE_THEME.to_string();
                    next.appearance.dark_theme = RESOLVE_THEME.to_string();
                    next.appearance.sync_color_theme();
                }
            }
            Workflow::Premiere => {
                let want = std::mem::take(&mut next.workflow_return_preset);
                // a hand-picked theme stays; the Resolve theme the switch set goes back to the one
                // Premiere had (the default when there is none); no record yet = the Resolve theme
                let auto = if next.workflow_auto_theme.is_empty() { RESOLVE_THEME } else { next.workflow_auto_theme.as_str() };
                if next.appearance.dark_theme != auto {
                    next.workflow_return_theme.clear();
                    next.workflow_auto_theme = HAND_PICKED_THEME.to_string();
                } else {
                    let back = std::mem::take(&mut next.workflow_return_theme);
                    let back = if DARK_THEMES.iter().any(|(v, _)| *v == back) { back } else { AppearancePrefs::default().dark_theme };
                    next.workflow_auto_theme = back.clone();
                    if next.appearance.dark_theme != back {
                        next.appearance.dark_theme = back;
                        next.appearance.sync_color_theme();
                    }
                }
                // another preset chosen by hand while in Resolve stays
                if self.shortcuts.preset == RESOLVE_PRESET {
                    if self.shortcuts.modified {
                        let name = self.shortcuts.free_preset_name(&format!("{RESOLVE_PRESET} (edited)"));
                        self.shortcuts.save_preset(&name).map_err(|e| bad(CMD, format!("could not keep your edited shortcuts as `{name}`: {e}")))?;
                    }
                    let want = if want.is_empty() || want == RESOLVE_PRESET { DEFAULT_PRESET.to_string() } else { want };
                    if self.shortcuts.load_preset(&want).is_err() {
                        self.shortcuts.load_preset(DEFAULT_PRESET).map_err(|e| bad(CMD, e))?;
                    }
                }
            }
        }
        next.workflow = w;
        self.set_prefs(next).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))
    }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        spec("settings.getWorkflow", "Get Workflow", "{}", |s, _| Ok(workflow_json(s)), false),
        spec(
            "settings.setWorkflow",
            "Set Workflow",
            r#"{"workflow":"premiere|resolve"}"#,
            |s, p| {
                let w = str_p(p, "workflow").and_then(Workflow::parse).ok_or_else(|| bad("settings.setWorkflow", "`workflow` must be premiere or resolve"))?;
                s.set_workflow(w)?;
                Ok(workflow_json(s))
            },
            false,
        ),
        spec("prefs.schema", "Settings Schema", r#"{"category":str?}"#, schema, false),
        spec("app.paths", "Application Folders", "{}", app_paths, false),
        spec("mediaCache.info", "Media Cache Info", "{}", cache_info, false),
        spec("mediaCache.clean", "Delete Media Cache Files", r#"{"all":bool?}"#, cache_clean, true),
    ]
}
