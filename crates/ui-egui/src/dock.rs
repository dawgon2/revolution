//! Premiere-style docking: a tree of splits and tab groups. Panels are rounded frames separated by
//! thin gutters; each group has a tab strip (active tab bright, with a panel menu "≡"); the focused
//! panel gets a blue outline. Gutters drag to resize; workspaces are serialized trees.

use egui::{Align2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};

use craft_ui::layout::{ResizeHandle, ResizeHandleStyle, SplitAxis, SplitSize as SharedSplitSize, split_rect};

use crate::icons::{self, Icon};
use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanelKind {
    Project,
    MediaBrowser,
    Libraries,
    Info,
    Effects,
    Markers,
    History,
    Source,
    EffectControls,
    AudioClipMixer,
    Metadata,
    Program,
    Timeline,
    Tools,
    AudioMeters,
    AudioTrackMixer,
    LumetriColor,
    LumetriScopes,
    EssentialGraphics,
    EssentialSound,
    TextToSpeech,
    Properties,
    Text,
    Events,
    Progress,
    ReferenceMonitor,
    Timecode,
    /// Color grading: the clip's serial node strip (fork GRADE-PLAN G1).
    ColorNodes,
    /// Color grading: primaries wheels / curves of the selected node.
    ColorPrimaries,
    /// Color grading: the selected node's curves and soft clip on their own (fork: the middle
    /// palette of Resolve's Color page, between the wheels and the scopes).
    ColorCurves,
    /// Color grading: the stills gallery (grab, compare, apply a still's grade).
    Gallery,
    ProjectNotes,
    Compositor,
    /// Color page: the sequence's clips as thumbnails over a mini timeline (Resolve's clip strip).
    ColorClips,
    /// Resolve workflow's Deliver page (fork RS3): the export settings as a panel. Not in
    /// [`PanelKind::ALL`], so the Premiere workflow's Window menu and shortcut list stay as they
    /// were; the page's toolbar opens and closes it.
    RenderSettings,
    /// Resolve workflow's Deliver page (fork RS3): the export queue as a panel (not in `ALL`).
    RenderQueue,
    /// Resolve workflow's Cut page (fork RS4): its fast-edit toolbar over the upper (whole) and
    /// lower (zoomed, playhead in the middle) timelines (not in `ALL`).
    CutTimeline,
    /// The assistant's chat (fork AI3, FORK-PLAN §1a.78): opened floating by the Assistant button
    /// in the page bar / header, dockable like any panel.
    Assistant,
    /// Every AI tool's model and work, with Stop per tool and Stop All (owner request 2026-10-24).
    AiTools,
    /// Resolve workflow's Inspector (fork RS4.2): the selected clip's settings in Resolve's
    /// layout (tabs, sections); the page toolbar's Inspector toggle opens it (not in `ALL`).
    Inspector,
}

impl PanelKind {
    pub const ALL: [PanelKind; 36] = [
        PanelKind::Project,
        PanelKind::MediaBrowser,
        PanelKind::Libraries,
        PanelKind::Info,
        PanelKind::Effects,
        PanelKind::Markers,
        PanelKind::History,
        PanelKind::Source,
        PanelKind::EffectControls,
        PanelKind::AudioClipMixer,
        PanelKind::Metadata,
        PanelKind::Program,
        PanelKind::Timeline,
        PanelKind::Tools,
        PanelKind::AudioMeters,
        PanelKind::AudioTrackMixer,
        PanelKind::LumetriColor,
        PanelKind::LumetriScopes,
        PanelKind::EssentialGraphics,
        PanelKind::EssentialSound,
        PanelKind::TextToSpeech,
        PanelKind::Properties,
        PanelKind::Text,
        PanelKind::Events,
        PanelKind::Progress,
        PanelKind::ReferenceMonitor,
        PanelKind::Timecode,
        PanelKind::ColorNodes,
        PanelKind::ColorPrimaries,
        PanelKind::Gallery,
        PanelKind::ProjectNotes,
        PanelKind::ColorClips,
        PanelKind::ColorCurves,
        PanelKind::Compositor,
        PanelKind::Assistant,
        PanelKind::AiTools,
    ];
    /// Panels only the Resolve workflow's pages use (fork RS3), kept out of [`PanelKind::ALL`].
    pub const RESOLVE_ONLY: [PanelKind; 4] = [PanelKind::RenderSettings, PanelKind::RenderQueue, PanelKind::CutTimeline, PanelKind::Inspector];
    pub fn title(self) -> &'static str {
        match self {
            PanelKind::Project => "Project",
            PanelKind::MediaBrowser => "Media Browser",
            PanelKind::Libraries => "Libraries",
            PanelKind::Info => "Info",
            PanelKind::Effects => "Effects",
            PanelKind::Markers => "Markers",
            PanelKind::History => "History",
            PanelKind::Source => "Source",
            PanelKind::EffectControls => "Effect Controls",
            PanelKind::AudioClipMixer => "Audio Clip Mixer",
            PanelKind::Metadata => "Metadata",
            PanelKind::Program => "Program",
            PanelKind::Timeline => "Timeline",
            PanelKind::Tools => "Tools",
            PanelKind::AudioMeters => "Audio Meters",
            PanelKind::AudioTrackMixer => "Audio Track Mixer",
            PanelKind::LumetriColor => "Lumetri Color",
            PanelKind::LumetriScopes => "Lumetri Scopes",
            PanelKind::EssentialGraphics => "Essential Graphics",
            PanelKind::EssentialSound => "Essential Sound",
            PanelKind::TextToSpeech => "Text to Speech",
            PanelKind::Properties => "Properties",
            PanelKind::Text => "Text",
            PanelKind::Events => "Events",
            PanelKind::Progress => "Progress",
            PanelKind::ReferenceMonitor => "Reference Monitor",
            PanelKind::Timecode => "Timecode",
            PanelKind::ColorNodes => "Nodes",
            PanelKind::ColorPrimaries => "Primaries",
            PanelKind::ColorCurves => "Curves",
            PanelKind::Gallery => "Gallery",
            PanelKind::ProjectNotes => "Project Notes",
            PanelKind::Compositor => "Compositor",
            PanelKind::ColorClips => "Clips",
            PanelKind::RenderSettings => "Render Settings",
            PanelKind::RenderQueue => "Render Queue",
            PanelKind::CutTimeline => "Cut Timeline",
            PanelKind::Assistant => "Assistant",
            PanelKind::AiTools => "AI Tools",
            PanelKind::Inspector => "Inspector",
        }
    }
    pub fn id(self) -> String {
        format!("{self:?}")
    }
    pub fn from_name(s: &str) -> Option<PanelKind> {
        let n = s.to_ascii_lowercase().replace([' ', '_', '-'], "");
        Self::ALL
            .iter()
            .chain(&Self::RESOLVE_ONLY)
            .copied()
            .find(|p| format!("{p:?}").to_ascii_lowercase() == n || p.title().to_ascii_lowercase().replace(' ', "") == n)
    }
    /// Window-menu shortcut (Premiere: Shift+1..9).
    pub fn window_shortcut(self) -> Option<&'static str> {
        match self {
            PanelKind::Project => Some("Shift+1"),
            PanelKind::Source => Some("Shift+2"),
            PanelKind::Timeline => Some("Shift+3"),
            PanelKind::Program => Some("Shift+4"),
            PanelKind::EffectControls => Some("Shift+5"),
            PanelKind::AudioClipMixer => Some("Shift+9"),
            PanelKind::Effects => Some("Shift+7"),
            PanelKind::MediaBrowser => Some("Shift+8"),
            PanelKind::AudioTrackMixer => Some("Shift+6"),
            _ => None,
        }
    }
    /// Narrow chrome-less panels (no tab strip; a slim grip instead).
    pub fn compact(self) -> bool {
        matches!(self, PanelKind::Tools | PanelKind::AudioMeters)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum SplitSize {
    Ratio(f32),
    /// First child has a fixed size in points.
    FixedA(f32),
    /// Second child has a fixed size in points.
    FixedB(f32),
}

impl SplitSize {
    fn shared(self) -> SharedSplitSize {
        match self {
            Self::Ratio(value) => SharedSplitSize::Ratio(value),
            Self::FixedA(value) => SharedSplitSize::FixedFirst(value),
            Self::FixedB(value) => SharedSplitSize::FixedSecond(value),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DockNode {
    /// `vertical`: children stacked top/bottom; else side by side.
    Split {
        vertical: bool,
        size: SplitSize,
        a: Box<DockNode>,
        b: Box<DockNode>,
    },
    Tabs {
        panels: Vec<PanelKind>,
        active: usize,
    },
}

fn tabs(p: &[PanelKind], active: usize) -> DockNode {
    DockNode::Tabs { panels: p.to_vec(), active }
}
fn hsplit(size: SplitSize, a: DockNode, b: DockNode) -> DockNode {
    DockNode::Split { vertical: false, size, a: Box::new(a), b: Box::new(b) }
}
fn vsplit(size: SplitSize, a: DockNode, b: DockNode) -> DockNode {
    DockNode::Split { vertical: true, size, a: Box::new(a), b: Box::new(b) }
}

pub const WORKSPACES: [&str; 11] =
    ["Editing", "Assembly", "Color", "Color Grading", "Effects", "Audio", "Captions and Graphics", "Compositing", "Learning", "Review", "All Panels"];

/// The Resolve workflow's page layouts (fork RS3), built-in workspaces listed only in that
/// workflow: (workspace, the Premiere-workflow workspace it stands for). The Color page is the
/// existing Color Grading workspace (GRADE-PLAN G1.8).
pub const PAGE_WORKSPACES: [(&str, &str); 5] =
    [("Media Page", "Assembly"), ("Cut Page", "Editing"), ("Edit Page", "Editing"), ("Fairlight Page", "Audio"), ("Deliver Page", "Editing")];

/// The user's workspaces file, beside `preferences.json` in the data directory.
pub const WORKSPACES_FILE: &str = "workspaces.json";

/// Window ▸ Workspaces: the user's own workspaces and saved changes to the built-in ones, and the
/// workspace in use (reopened at launch).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspacePrefs {
    pub saved: Vec<SavedWorkspace>,
    pub current: String,
    /// 1 once the user's own workspaces were checked against the page workspaces' names (fork
    /// RS3, [`WorkspacePrefs::migrate_page_names`]); a file written before RS3 reads 0.
    #[serde(default)]
    pub pages_version: u32,
    /// Fork LAY.2: each workspace's arrangement as it was last left when that differs from its
    /// saved layout, so switching away and back (or quitting) keeps unsaved changes, as Premiere
    /// and Resolve do; Reset to Saved Layout drops it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub working: Vec<SavedWorkspace>,
    /// Fork LAY.3: Window ▸ Layout Presets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<crate::panels::layout_presets::LayoutPreset>,
}

impl Default for WorkspacePrefs {
    fn default() -> Self {
        WorkspacePrefs { saved: vec![], current: String::new(), pages_version: 1, working: vec![], presets: vec![] }
    }
}

/// A named layout. `layout` is a [`DockNode`] kept as JSON, so one unreadable entry (a panel a
/// newer version added) doesn't lose the others.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedWorkspace {
    pub name: String,
    pub layout: serde_json::Value,
    /// Kept raw for the same per-workspace failure isolation as the dock tree.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub floating: serde_json::Value,
}

impl WorkspacePrefs {
    pub fn load(path: &std::path::Path) -> Self {
        use std::io::Read as _;
        // Preferences are untrusted; bound reading before JSON can allocate from it.
        const MAX_BYTES: u64 = 4 * 1024 * 1024;
        let Some(bytes) = std::fs::File::open(path).ok().and_then(|file| {
            let mut bytes = Vec::new();
            file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
            (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
        }) else {
            return Self::default();
        };
        let mut prefs: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        prefs.migrate_page_names();
        prefs
    }
    /// Fork RS3: a workspace the user saved before RS3 under a name the pages now use ("Edit
    /// Page"…) would be hidden behind the built-in page; rename it "<name> (custom)" (with a
    /// number when that is taken too) once, keeping it the current workspace if it was.
    pub fn migrate_page_names(&mut self) {
        if self.pages_version >= 1 {
            return;
        }
        self.pages_version = 1;
        for i in 0..self.saved.len() {
            let Some(name) = self.saved.get(i).map(|s| s.name.clone()) else { continue };
            if !PAGE_WORKSPACES.iter().any(|(w, _)| *w == name) {
                continue;
            }
            let taken = |n: &str, saved: &[SavedWorkspace]| is_builtin(n) || saved.iter().any(|s| s.name == n);
            let mut new = format!("{name} (custom)");
            let mut k = 2_u32;
            while taken(&new, &self.saved) && k < 1000 {
                new = format!("{name} (custom {k})");
                k = k.saturating_add(1);
            }
            if self.current == name {
                self.current = new.clone();
            }
            if let Some(s) = self.saved.get_mut(i) {
                s.name = new;
            }
        }
    }
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, path)
    }
}

/// A workspace's command-id key: `Captions and Graphics` → `captionsandgraphics`.
pub fn slug(name: &str) -> String {
    name.to_ascii_lowercase().replace(' ', "")
}

pub fn is_builtin(name: &str) -> bool {
    WORKSPACES.contains(&name) || PAGE_WORKSPACES.iter().any(|(w, _)| *w == name)
}

/// Every workspace, built-in and the user's own, in one alphabetical list (Premiere's Window ▸
/// Workspaces). The Premiere workflow's list; see [`names_for`].
pub fn names(prefs: &WorkspacePrefs) -> Vec<String> {
    names_for(prefs, false)
}

/// [`names`] for a workflow: in the Resolve workflow (fork RS3) the page workspaces replace the
/// Premiere ones they stand for (Assembly, Editing, Audio, Color), since choosing one of those
/// lands on its page; the Premiere workflow never lists the page workspaces.
pub fn names_for(prefs: &WorkspacePrefs, resolve: bool) -> Vec<String> {
    let page_of = |n: &str| PAGE_WORKSPACES.iter().any(|(w, _)| *w == n);
    let mut v: Vec<String> = if resolve {
        WORKSPACES
            .iter()
            .filter(|w| !matches!(**w, "Assembly" | "Editing" | "Audio" | "Color"))
            .chain(PAGE_WORKSPACES.iter().map(|(w, _)| w))
            .map(|s| s.to_string())
            .collect()
    } else {
        WORKSPACES.iter().map(|s| s.to_string()).collect()
    };
    let saved = prefs.saved.iter().filter(|s| resolve || !page_of(&s.name));
    let saved: Vec<String> = saved.map(|s| s.name.clone()).collect();
    for s in saved {
        if !v.contains(&s) && !(resolve && matches!(s.as_str(), "Assembly" | "Editing" | "Audio" | "Color")) {
            v.push(s);
        }
    }
    v.sort_by_key(|n| n.to_lowercase());
    v
}

/// The workspace a name means in a workflow (fork RS3; the Color rule is GRADE-PLAN G1.8): in the
/// Resolve workflow Assembly, Editing, Audio and Color open their pages; in the Premiere workflow
/// the page workspaces open the Premiere workspace they stand for.
pub fn for_workflow(name: &str, resolve: bool) -> &str {
    if resolve {
        match name {
            "Assembly" => "Media Page",
            "Editing" => "Edit Page",
            "Audio" => "Fairlight Page",
            "Color" => "Color Grading",
            n => n,
        }
    } else {
        PAGE_WORKSPACES.iter().find(|(w, _)| *w == name).map_or(name, |(_, premiere)| premiere)
    }
}

/// The workspace called `query` (any case, spaces optional).
pub fn find(prefs: &WorkspacePrefs, query: &str) -> Option<String> {
    names(prefs).into_iter().chain(names_for(prefs, true)).find(|n| slug(n) == slug(query))
}

/// The saved layout of a workspace: the user's saved version, else the built-in default.
pub fn saved_layout(prefs: &WorkspacePrefs, name: &str) -> DockNode {
    saved_docking(prefs, name).root
}

/// The layout a workspace opens with: its unsaved arrangement (fork LAY.2) when there is a
/// readable one, else [`saved_docking`].
pub fn current_docking(prefs: &WorkspacePrefs, name: &str) -> Layout {
    prefs.working.iter().find(|w| w.name == name).and_then(read_layout).unwrap_or_else(|| saved_docking(prefs, name))
}

/// A stored layout, or `None` when it doesn't read or validate (a panel a newer version added).
pub fn read_layout(saved: &SavedWorkspace) -> Option<Layout> {
    let layout = Layout {
        root: serde_json::from_value(saved.layout.clone()).ok()?,
        floating: if saved.floating.is_null() { vec![] } else { serde_json::from_value(saved.floating.clone()).ok()? },
    };
    layout.validate().ok().map(|()| layout)
}

impl WorkspacePrefs {
    /// Fork LAY.2: remember `layout` as `name`'s unsaved arrangement, or forget it when it is the
    /// saved layout. Whether anything changed.
    pub fn keep_working(&mut self, name: &str, layout: &Layout) -> bool {
        let before = self.working.clone();
        self.working.retain(|w| w.name != name);
        let to_json = |l: &Layout| (serde_json::to_value(&l.root).ok(), serde_json::to_value(&l.floating).ok());
        if !name.is_empty() && layout.validate().is_ok() && to_json(layout) != to_json(&saved_docking(self, name)) {
            let (Some(root), Some(floating)) = (serde_json::to_value(&layout.root).ok(), serde_json::to_value(&layout.floating).ok()) else {
                return self.working != before;
            };
            let floating = if layout.floating.is_empty() { serde_json::Value::Null } else { floating };
            self.working.push(SavedWorkspace { name: name.to_string(), layout: root, floating });
            // bounded: an old entry goes first
            const MAX_WORKING: usize = 64;
            if self.working.len() > MAX_WORKING {
                let extra = self.working.len() - MAX_WORKING;
                self.working.drain(..extra);
            }
        }
        self.working != before
    }

    /// Fork LAY.2: forget `name`'s unsaved arrangement (saved, reset, deleted or renamed).
    pub fn drop_working(&mut self, name: &str) {
        self.working.retain(|w| w.name != name);
    }
}

pub fn saved_docking(prefs: &WorkspacePrefs, name: &str) -> Layout {
    let saved = prefs.saved.iter().find(|saved| saved.name == name).and_then(|saved| {
        let mut layout = Layout {
            root: serde_json::from_value(saved.layout.clone()).ok()?,
            floating: if saved.floating.is_null() { vec![] } else { serde_json::from_value(saved.floating.clone()).ok()? },
        };
        if layout.floating.is_empty() {
            migrate_legacy_effects(&mut layout.root);
        }
        layout.validate().ok().map(|()| layout)
    });
    saved.unwrap_or_else(|| Layout { root: workspace(name), floating: vec![] })
}

/// Older Effects workspaces stored the same Effect Controls panel twice: selected beside
/// Source, and an inactive duplicate beside Essential Graphics. Remove only that known
/// duplicate shape; unrelated malformed or ambiguous saved layouts still fail validation.
fn migrate_legacy_effects(root: &mut DockNode) {
    let Ok(shared) = crate::dock_shared::encode(root, &[]) else { return };
    if shared.panels().iter().filter(|panel| ***panel == PanelKind::EffectControls).count() != 2 {
        return;
    }
    fn selected_source(node: &DockNode) -> bool {
        match node {
            DockNode::Tabs { panels, active } => {
                panels.contains(&PanelKind::Source) && panels.contains(&PanelKind::AudioClipMixer) && panels.get(*active) == Some(&PanelKind::EffectControls)
            }
            DockNode::Split { a, b, .. } => selected_source(a) || selected_source(b),
        }
    }
    fn remove_inactive(node: &mut DockNode) {
        match node {
            DockNode::Tabs { panels, active } if panels.as_slice() == [PanelKind::EffectControls, PanelKind::EssentialGraphics] && *active == 1 => {
                panels.remove(0);
                *active = 0;
            }
            DockNode::Split { a, b, .. } => {
                remove_inactive(a);
                remove_inactive(b);
            }
            DockNode::Tabs { .. } => {}
        }
    }
    if selected_source(root) {
        remove_inactive(root);
    }
}

/// The default layout of a named workspace.
pub fn workspace(name: &str) -> DockNode {
    use PanelKind::*;
    use SplitSize::*;
    let bottom_editing = || hsplit(FixedA(64.0), tabs(&[Tools], 0), hsplit(FixedB(116.0), tabs(&[Timeline], 0), tabs(&[AudioMeters], 0)));
    match name {
        "Assembly" => vsplit(
            Ratio(0.52),
            hsplit(
                Ratio(0.46),
                tabs(&[Project, MediaBrowser, Libraries, Info, Effects, Markers, History], 0),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer, Metadata], 0), tabs(&[Program], 0)),
            ),
            bottom_editing(),
        ),
        "Color" => hsplit(
            FixedB(340.0),
            vsplit(
                Ratio(0.58),
                hsplit(Ratio(0.42), tabs(&[LumetriScopes, Source, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, Info, Markers, History], 0), bottom_editing()),
            ),
            tabs(&[LumetriColor, Effects], 0),
        ),
        // fork GRADE-PLAN G1: the node grader, laid out like Resolve's Color page: gallery |
        // viewer | nodes on top; the clips (thumbnails over a mini timeline, the full Timeline one
        // tab away) across the whole width, with no tools or meters beside them; primaries /
        // curves and scopes below
        "Color Grading" => vsplit(
            Ratio(0.46),
            hsplit(
                FixedA(250.0),
                tabs(&[Gallery, Project, MediaBrowser], 0),
                hsplit(FixedB(400.0), tabs(&[Program, Source, ReferenceMonitor], 0), tabs(&[ColorNodes, Info, History], 0)),
            ),
            vsplit(
                FixedA(150.0),
                tabs(&[ColorClips, Timeline], 0),
                // wheels | curves | scopes, as Resolve's Color page
                hsplit(
                    Ratio(0.47),
                    tabs(&[ColorPrimaries, LumetriColor, EffectControls], 0),
                    hsplit(Ratio(0.5), tabs(&[ColorCurves], 0), tabs(&[LumetriScopes, Markers], 0)),
                ),
            ),
        ),
        "Effects" => hsplit(
            FixedB(320.0),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer], 1), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Info, Markers, History], 0), bottom_editing()),
            ),
            vsplit(Ratio(0.5), tabs(&[Effects], 0), tabs(&[EssentialGraphics], 0)),
        ),
        "Audio" => hsplit(
            FixedB(320.0),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.5), tabs(&[AudioTrackMixer, Source, AudioClipMixer, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Effects, Markers, History], 0), bottom_editing()),
            ),
            tabs(&[EssentialSound, TextToSpeech], 0),
        ),
        // fork RS3: the Resolve workflow's pages (RESOLVE-UI-PLAN RS3, from observation of
        // Resolve 21's page layouts). Media: the file browser and the viewer over the media pool,
        // metadata on the right.
        "Media Page" => hsplit(
            FixedB(320.0),
            vsplit(Ratio(0.55), hsplit(Ratio(0.42), tabs(&[MediaBrowser], 0), tabs(&[Source], 0)), tabs(&[Project], 0)),
            vsplit(Ratio(0.7), tabs(&[Metadata, Info], 0), tabs(&[AudioMeters], 0)),
        ),
        // fork RS4, the Cut page (from observation of Resolve 21's): the media pool and the
        // timeline viewer side by side over the Cut timeline (fast edits, the whole timeline,
        // the zoomed-in timeline), the audio meters beside the viewer.
        "Cut Page" => vsplit(
            Ratio(0.5),
            hsplit(Ratio(0.5), tabs(&[Project], 0), hsplit(FixedB(40.0), tabs(&[Program], 0), tabs(&[AudioMeters], 0))),
            tabs(&[CutTimeline], 0),
        ),
        // Edit: the media pool (bins) down the left; source and timeline viewers side by side over
        // the timeline. The Inspector opens on the right from the page's toolbar.
        "Edit Page" => hsplit(
            FixedA(440.0),
            tabs(&[Project, Effects, Markers], 0),
            // fork RS4: Resolve's timeline toolbar holds the tools (no Tools panel)
            vsplit(Ratio(0.5), hsplit(Ratio(0.5), tabs(&[Source], 0), tabs(&[Program], 0)), tabs(&[Timeline], 0)),
        ),
        // Fairlight: a strip across the top (a small viewer, then the meters with the loudness
        // readout), the timeline under it, the mixer on the right.
        "Fairlight Page" => vsplit(
            FixedA(270.0),
            hsplit(FixedB(220.0), tabs(&[Program], 0), tabs(&[AudioMeters], 0)),
            hsplit(FixedB(360.0), tabs(&[Timeline], 0), tabs(&[AudioTrackMixer, EssentialSound], 0)),
        ),
        // Deliver: render settings down the left; the viewer and the render queue over the
        // timeline.
        "Deliver Page" => hsplit(
            FixedA(450.0),
            tabs(&[RenderSettings], 0),
            vsplit(Ratio(0.55), hsplit(FixedB(420.0), tabs(&[Program], 0), tabs(&[RenderQueue], 0)), tabs(&[Timeline], 0)),
        ),
        // fork C1: the Compositor large beside the Program monitor, the project and the timeline
        // under them (Window ▸ Workspaces ▸ Compositing; Resolve workflow: the Fusion page)
        "Compositing" => vsplit(
            Ratio(0.58),
            hsplit(Ratio(0.55), tabs(&[Compositor], 0), tabs(&[Program], 0)),
            hsplit(Ratio(0.3), tabs(&[Project, EffectControls, Effects], 0), bottom_editing()),
        ),
        "Captions and Graphics" => hsplit(
            FixedB(330.0),
            vsplit(
                Ratio(0.5),
                hsplit(Ratio(0.4), tabs(&[Text, Source, EffectControls], 0), tabs(&[Program], 0)),
                hsplit(Ratio(0.3), tabs(&[Project, MediaBrowser, Libraries], 0), bottom_editing()),
            ),
            tabs(&[EssentialGraphics, Properties], 0),
        ),
        "All Panels" => vsplit(
            Ratio(0.5),
            hsplit(
                Ratio(0.5),
                tabs(&[Source, EffectControls, AudioClipMixer, Metadata, LumetriScopes, Text], 0),
                hsplit(
                    Ratio(0.6),
                    tabs(&[Program], 0),
                    tabs(&[LumetriColor, ColorPrimaries, ColorNodes, Gallery, EssentialGraphics, EssentialSound, TextToSpeech, Properties, AudioTrackMixer], 0),
                ),
            ),
            hsplit(Ratio(0.26), tabs(&[Project, MediaBrowser, Libraries, Info, Effects, Markers, History], 0), bottom_editing()),
        ),
        // Editing (default), Learning, Review — Premiere 26 factory layout:
        // top: Source group | Program | Properties; bottom: Project group | Tools | Timeline | Meters.
        _ => vsplit(
            Ratio(0.57),
            hsplit(
                FixedB(450.0),
                hsplit(Ratio(0.5), tabs(&[Source, EffectControls, AudioClipMixer, Metadata], 0), tabs(&[Program], 0)),
                tabs(&[Properties, EssentialGraphics, Text], 0),
            ),
            hsplit(
                Ratio(0.29),
                tabs(&[Project, MediaBrowser, Libraries, Info, Effects, Markers, History], 0),
                hsplit(FixedA(64.0), tabs(&[Tools], 0), hsplit(FixedB(116.0), tabs(&[Timeline], 0), tabs(&[AudioMeters], 0))),
            ),
        ),
    }
}

impl DockNode {
    /// Every panel in the tree.
    pub fn panels(&self, out: &mut Vec<PanelKind>) {
        match self {
            DockNode::Split { a, b, .. } => {
                a.panels(out);
                b.panels(out);
            }
            DockNode::Tabs { panels, .. } => out.extend(panels.iter().copied()),
        }
    }
    pub fn contains(&self, p: PanelKind) -> bool {
        let mut v = Vec::new();
        self.panels(&mut v);
        v.contains(&p)
    }
    /// Make `p` the active tab of its group. Returns false if not present.
    pub fn activate(&mut self, p: PanelKind) -> bool {
        crate::dock_shared::apply_tree(self, craft_ui::docking::Action::Activate { panel: p })
    }
    pub fn is_visible(&self, p: PanelKind) -> bool {
        match self {
            DockNode::Split { a, b, .. } => a.is_visible(p) || b.is_visible(p),
            DockNode::Tabs { panels, active } => panels.get(*active) == Some(&p),
        }
    }
    /// Close a panel (remove its tab; empty groups collapse their split).
    pub fn close(&mut self, p: PanelKind) {
        crate::dock_shared::apply_tree(self, craft_ui::docking::Action::Close { panel: p });
    }
    /// Put the Timeline panel back where the workspaces keep it (left of the audio meters) after
    /// it was closed, so opening a sequence always has somewhere to show it.
    pub fn restore_timeline(&mut self) {
        if self.contains(PanelKind::Timeline) {
            return;
        }
        fn beside_meters(n: &mut DockNode) -> bool {
            match n {
                DockNode::Split { a, b, .. } => beside_meters(a) || beside_meters(b),
                DockNode::Tabs { panels, .. } if panels.as_slice() == [PanelKind::AudioMeters] => {
                    *n = hsplit(SplitSize::FixedB(116.0), tabs(&[PanelKind::Timeline], 0), tabs(&[PanelKind::AudioMeters], 0));
                    true
                }
                DockNode::Tabs { .. } => false,
            }
        }
        if !beside_meters(self) {
            self.open_near(PanelKind::Timeline, PanelKind::Project);
        }
    }
    /// Add a panel as a column of its own (`width` points) on the left or right of the whole tree
    /// (fork RS3: a page toolbar's panel toggles). Nothing happens when it is already there or
    /// the tree is malformed.
    pub fn add_side(&mut self, p: PanelKind, left: bool, width: f32) {
        if self.contains(p) || crate::dock_shared::encode(self, &[]).is_err() {
            return;
        }
        let rest = std::mem::replace(self, tabs(&[], 0));
        let side = tabs(&[p], 0);
        *self = if matches!(&rest, DockNode::Tabs { panels, .. } if panels.is_empty()) {
            side
        } else if left {
            hsplit(SplitSize::FixedA(width), side, rest)
        } else {
            hsplit(SplitSize::FixedB(width), rest, side)
        };
    }
    /// Add a panel as a tab next to `near` (or into the first group).
    pub fn open_near(&mut self, p: PanelKind, near: PanelKind) {
        if crate::dock_shared::encode(self, &[]).is_err() {
            return;
        }
        let anchor = [near, PanelKind::Project, PanelKind::Program].into_iter().find(|panel| self.contains(*panel)).or_else(|| {
            let mut panels = Vec::new();
            self.panels(&mut panels);
            panels.first().copied()
        });
        crate::dock_shared::apply_tree(self, craft_ui::docking::Action::Open { panel: p, anchor });
    }
}

/// A floating group retains the same panel identities as docked tabs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Floating {
    pub panels: Vec<PanelKind>,
    pub active: usize,
    /// [x, y, width, height] in logical points.
    pub rect: [f32; 4],
}

/// App persistence shape; all mutations pass through the shared docking model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub root: DockNode,
    #[serde(default)]
    pub floating: Vec<Floating>,
}

impl Layout {
    pub fn apply(&mut self, action: craft_ui::docking::Action<PanelKind>) -> Result<(), String> {
        crate::dock_shared::apply(self, action)
    }
    pub fn validate(&self) -> Result<(), String> {
        crate::dock_shared::encode(&self.root, &self.floating)?.validate().map_err(|error| error.to_string())
    }
    pub fn contains(&self, panel: PanelKind) -> bool {
        self.root.contains(panel) || self.floating.iter().any(|group| group.panels.contains(&panel))
    }
}

/// One laid-out tab group.
pub struct Group {
    pub path: String,
    pub rect: Rect,
    pub content: Rect,
    pub panels: Vec<PanelKind>,
    pub active: usize,
}

/// Actions produced by interacting with the dock chrome.
#[derive(Debug, Clone, PartialEq)]
pub enum DockAction {
    Activate(PanelKind),
    BeginDrag(PanelKind),
    Focus(PanelKind),
    Close(PanelKind),
    PanelMenu(PanelKind, egui::Pos2),
    /// A sequence tab of the Timeline panel was clicked / closed (project item id).
    OpenSequence(u64),
    CloseSequence(u64),
    /// A sequence tab was dragged past a neighbour: its new place among the sequence tabs.
    MoveSequence(u64, usize),
}

/// Lay out the tree into group rects (with `gap` gutters) and handle gutter dragging.
pub fn layout(ui: &mut egui::Ui, node: &mut DockNode, rect: Rect, t: &Tokens, path: &str, out: &mut Vec<Group>, reg: &mut crate::automation::Registry) {
    let mut actions = Vec::new();
    layout_with_actions(ui, node, rect, t, path, out, reg, &mut actions);
    for action in actions {
        crate::dock_shared::apply_tree(node, action);
    }
}

/// Lay out chrome while returning mutations for the application's UI command route.
pub fn layout_with_actions(
    ui: &mut egui::Ui,
    node: &mut DockNode,
    rect: Rect,
    t: &Tokens,
    path: &str,
    out: &mut Vec<Group>,
    reg: &mut crate::automation::Registry,
    actions: &mut Vec<craft_ui::docking::Action<PanelKind>>,
) {
    layout_inner(ui, node, rect, t, path, &[], out, reg, actions);
}

fn layout_inner(
    ui: &mut egui::Ui,
    node: &mut DockNode,
    rect: Rect,
    t: &Tokens,
    path: &str,
    tree_path: &[bool],
    out: &mut Vec<Group>,
    reg: &mut crate::automation::Registry,
    actions: &mut Vec<craft_ui::docking::Action<PanelKind>>,
) {
    match node {
        DockNode::Tabs { panels, active } => {
            let tab_h = if panels.len() == 1 && panels[0].compact() { 8.0 } else { t.tab_h };
            let content = Rect::from_min_max(pos2(rect.min.x, rect.min.y + tab_h), rect.max);
            out.push(Group { path: path.to_string(), rect, content, panels: panels.clone(), active: *active });
        }
        DockNode::Split { vertical, size, a, b } => {
            let axis = if *vertical { SplitAxis::Vertical } else { SplitAxis::Horizontal };
            let Some(split) = split_rect(rect, axis, size.shared(), t.gap, 20.0) else { return };
            let id = egui::Id::new(("dock-gutter", path.to_string()));
            let handle_style = ResizeHandleStyle { hovered: t.focus.gamma_multiply(0.4), dragged: t.focus.gamma_multiply(0.9), focused: t.focus };
            let label = if *vertical { tl!("Height:") } else { tl!("Width:") };
            let response = ResizeHandle::new(id, axis, split.divider, label, handle_style).value(split.first_size(), 0.0..=split.available()).show(ui);
            reg.add_response(&format!("dock.gutter.{path}"), &response.response, "gutter");
            if response.delta != 0.0
                && let Some(resized) = split.resized(size.shared(), response.delta, 40.0)
            {
                actions.push(craft_ui::docking::Action::ResizeSplit { path: tree_path.to_vec(), size: resized });
            }
            let mut first_path = tree_path.to_vec();
            first_path.push(false);
            let mut second_path = tree_path.to_vec();
            second_path.push(true);
            layout_inner(ui, a, split.first, t, &format!("{path}a"), &first_path, out, reg, actions);
            layout_inner(ui, b, split.second, t, &format!("{path}b"), &second_path, out, reg, actions);
        }
    }
}

/// Draw a group's frame + tab strip. Returns actions (tab clicks, panel menu, focus).
/// The sequences open in the Timeline panel, in tab order: `(project item id, name)`.
#[derive(Debug, Clone, Default)]
pub struct SeqTabs {
    pub open: Vec<(u64, String)>,
    pub active: Option<u64>,
}

pub fn draw_group_chrome(
    ui: &mut egui::Ui,
    g: &Group,
    focused: PanelKind,
    seqs: &SeqTabs,
    t: &Tokens,
    reg: &mut crate::automation::Registry,
) -> Vec<DockAction> {
    draw_group_chrome_details(ui, g, focused, seqs, t, reg).actions
}

#[derive(Default)]
pub struct Chrome {
    pub actions: Vec<DockAction>,
    pub tabs: Vec<(PanelKind, Rect)>,
}

pub fn draw_group_chrome_details(
    ui: &mut egui::Ui,
    g: &Group,
    focused: PanelKind,
    seqs: &SeqTabs,
    t: &Tokens,
    reg: &mut crate::automation::Registry,
) -> Chrome {
    let mut actions = Vec::new();
    let mut tab_rects: Vec<(PanelKind, Rect)> = Vec::new();
    let painter = ui.painter().clone();
    painter.rect_filled(g.rect, t.radius, t.panel_bg);
    let active_panel = g.panels.get(g.active).copied();
    let compact = g.panels.len() == 1 && g.panels[0].compact();
    // A narrow header grip moves the entire panel, including Timeline panels whose tabs
    // separately reorder open sequences.
    if let Some(panel) = active_panel {
        let grip = Rect::from_min_size(g.rect.min, vec2(if compact { g.rect.width() } else { 10.0 }, if compact { 8.0 } else { t.tab_h }));
        let response = ui.interact(grip, egui::Id::new(("panel-drag", g.path.clone())), Sense::drag());
        reg.add_response(&format!("panel.drag.{}", panel.id()), &response, "Move panel");
        if response.drag_started() {
            actions.push(DockAction::BeginDrag(panel));
        }
    }
    if compact {
        // grip dots
        let c = pos2(g.rect.center().x, g.rect.min.y + 4.0);
        for dx in [-4.0, 0.0, 4.0] {
            painter.circle_filled(c + vec2(dx, 0.0), 1.0, t.text_faint);
        }
    } else {
        let strip = Rect::from_min_size(g.rect.min, vec2(g.rect.width(), t.tab_h));
        // the Timeline panel shows one tab per open sequence (its own tab names the sequence)
        struct Tab {
            panel: PanelKind,
            seq: Option<u64>,
            title: String,
            active: bool,
        }
        let mut tabs: Vec<Tab> = Vec::new();
        for (i, p) in g.panels.iter().enumerate() {
            let active = i == g.active;
            if *p == PanelKind::Timeline && active && !seqs.open.is_empty() {
                tabs.extend(seqs.open.iter().map(|(id, name)| Tab { panel: *p, seq: Some(*id), title: name.clone(), active: seqs.active == Some(*id) }));
            } else if *p == PanelKind::Timeline {
                let name = seqs.open.iter().find(|(id, _)| seqs.active == Some(*id)).map(|(_, n)| n.clone());
                tabs.push(Tab { panel: *p, seq: None, title: name.unwrap_or_else(|| tl!("Timeline: (no sequences)").into()), active });
            } else {
                tabs.push(Tab { panel: *p, seq: None, title: crate::i18n::t(p.title()).to_string(), active });
            }
        }
        let activate = |tab: &Tab, actions: &mut Vec<DockAction>| match tab.seq {
            Some(id) => actions.push(DockAction::OpenSequence(id)),
            None => actions.push(DockAction::Activate(tab.panel)),
        };
        let active_at = tabs.iter().position(|tab| tab.active).unwrap_or(0);
        let text_y = strip.min.y + 16.0;
        // Which tabs fit. The active tab is always shown: tabs before it are left out until it
        // fits, then as many after it as there is room for. Tabs that are left out are reached
        // through the » list at the right end of the strip.
        let widths: Vec<f32> = tabs
            .iter()
            .map(|tab| {
                let label = painter.layout_no_wrap(tab.title.clone(), Tokens::ui(12.0), t.tab_text).size().x;
                label + 16.0 + if tab.active { 20.0 } else { 0.0 } + if tab.active && tab.seq.is_some() { 16.0 } else { 0.0 }
            })
            .collect();
        let span = |from: usize, to: usize| widths.get(from..=to).map_or(0.0, |w| w.iter().sum::<f32>() + 8.0 * w.len() as f32);
        let room = strip.width() - 12.0;
        let all_fit = span(0, tabs.len().saturating_sub(1)) <= room;
        let room = if all_fit { room } else { room - 24.0 };
        let mut first = 0;
        while first < active_at && span(first, active_at) > room {
            first += 1;
        }
        let mut last = active_at.max(first);
        while last + 1 < tabs.len() && span(first, last + 1) <= room {
            last += 1;
        }
        let list_id = egui::Id::new(("tab-list", g.path.clone()));
        let more = (!all_fit).then(|| Rect::from_min_size(pos2(strip.max.x - 22.0, strip.min.y + 6.0), vec2(18.0, 20.0)));
        let mut list_opened = false;
        if let Some(r) = more {
            let resp = ui.interact(r, egui::Id::new(("tab-overflow", g.path.clone())), Sense::click());
            let name = if g.panels.contains(&PanelKind::Timeline) { "timeline.tabs.more".to_string() } else { format!("panel.tabs.more.{}", g.path) };
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, resp.enabled(), "more tabs"));
            reg.add_response(&name, &resp, "more tabs");
            let c = if resp.hovered() { t.tab_text_active } else { t.tab_text };
            icons::paint(&painter, r.shrink(4.0).translate(vec2(-2.0, 0.0)), Icon::ChevronRight, c);
            icons::paint(&painter, r.shrink(4.0).translate(vec2(2.0, 0.0)), Icon::ChevronRight, c);
            paint_widget_focus(&painter, &resp, t);
            if resp.clicked() {
                let open = ui.ctx().data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false);
                ui.ctx().data_mut(|d| d.insert_temp(list_id, !open));
                list_opened = !open;
            }
        }
        let mut x = strip.min.x + 12.0;
        // sequence tabs as drawn (for dragging one past its neighbours), and the one being dragged
        let mut seq_rects: Vec<(u64, Rect)> = Vec::new();
        let mut dragged: Option<u64> = None;
        for (i, tab) in tabs.iter().enumerate().take(last + 1).skip(first) {
            let p = &tab.panel;
            let is_active = tab.active;
            let galley = painter.layout_no_wrap(tab.title.clone(), Tokens::ui(12.0), if is_active { t.tab_text_active } else { t.tab_text });
            let close_w = if is_active && tab.seq.is_some() { 16.0 } else { 0.0 };
            let w = widths.get(i).copied().unwrap_or(0.0);
            let tab_rect = Rect::from_min_size(pos2(x, strip.min.y), vec2(w, t.tab_h));
            // a sequence tab keeps its identity when the tabs are reordered under the pointer
            let resp = match tab.seq {
                Some(id) => ui.interact(tab_rect, egui::Id::new(("tab-seq", g.path.clone(), id)), Sense::click_and_drag()),
                None => ui.interact(tab_rect, egui::Id::new(("tab", g.path.clone(), i)), Sense::click_and_drag()),
            };
            if tab.seq.is_none() && resp.drag_started() {
                actions.push(DockAction::BeginDrag(*p));
            }
            if let Some((_, rect)) = tab_rects.iter_mut().find(|(panel, _)| panel == p) {
                *rect = rect.union(tab_rect);
            } else {
                tab_rects.push((*p, tab_rect));
            }
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, resp.enabled(), is_active, &tab.title));
            ui.ctx().accesskit_node_builder(resp.id, |node| {
                node.set_role(egui::accesskit::Role::Tab);
                node.set_selected(is_active);
            });
            paint_widget_focus(&painter, &resp, t);
            if let Some(id) = tab.seq {
                seq_rects.push((id, tab_rect));
                if resp.dragged() {
                    dragged = Some(id);
                }
            }
            if let Some(id) = tab.seq {
                reg.add_response(&format!("timeline.tab.{id}"), &resp, &tab.title);
            }
            if tab.seq.is_none() || is_active {
                reg.add_response(&format!("panel.tab.{}", p.id()), &resp, p.title());
            }
            let label_x = tab_rect.min.x + 8.0 + close_w;
            let label_w = galley.size().x;
            let col = if is_active || resp.hovered() { t.tab_text_active } else { t.tab_text };
            painter.galley_with_override_text_color(pos2(label_x, text_y - galley.size().y / 2.0), galley, col);
            let mut closed = false;
            if let Some(id) = tab.seq.filter(|_| is_active) {
                // × closes the sequence's tab (the sequence stays in the project)
                let cr = Rect::from_center_size(pos2(tab_rect.min.x + 10.0, text_y), vec2(8.0, 8.0));
                let cresp = ui.interact(cr.expand(4.0), egui::Id::new(("tab-close", g.path.clone(), id)), Sense::click());
                cresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, cresp.enabled(), "close sequence"));
                reg.add_response(&format!("timeline.tab.{id}.close"), &cresp, "close sequence");
                let cc = if cresp.hovered() { t.tab_text_active } else { t.tab_text };
                painter.line_segment([cr.left_top(), cr.right_bottom()], Stroke::new(1.2, cc));
                painter.line_segment([cr.right_top(), cr.left_bottom()], Stroke::new(1.2, cc));
                paint_widget_focus(&painter, &cresp, t);
                if cresp.clicked() {
                    actions.push(DockAction::CloseSequence(id));
                    closed = true;
                }
            }
            if is_active {
                let mr = Rect::from_center_size(pos2(label_x + label_w + 12.0, text_y), vec2(12.0, 10.0));
                let mresp = ui.interact(mr.expand(3.0), egui::Id::new(("tab-menu", g.path.clone())), Sense::click());
                mresp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, mresp.enabled(), "panel menu"));
                reg.add_response(&format!("panel.menu.{}", p.id()), &mresp, "panel menu");
                let mc = if mresp.hovered() { t.tab_text_active } else { t.tab_text };
                icons::paint(&painter, Rect::from_center_size(mr.center(), vec2(16.0, 16.0)), Icon::Hamburger, mc);
                // 1 pt underline spanning label + ≡, 23 pt below the frame top
                let uy = strip.min.y + 23.0;
                painter.line_segment([pos2(label_x, uy), pos2(mr.max.x, uy)], Stroke::new(1.0, t.tab_text_active));
                paint_widget_focus(&painter, &mresp, t);
                if mresp.clicked() {
                    actions.push(DockAction::PanelMenu(*p, mr.left_bottom()));
                }
            }
            if (resp.clicked() || resp.drag_started()) && !closed {
                activate(tab, &mut actions);
                actions.push(DockAction::Focus(*p));
            }
            // a right-click shows the tab and opens its panel menu (as in Premiere)
            if resp.secondary_clicked() {
                activate(tab, &mut actions);
                actions.push(DockAction::Focus(*p));
                let at = resp.interact_pointer_pos().unwrap_or(tab_rect.left_bottom());
                actions.push(DockAction::PanelMenu(*p, at));
            }
            if resp.middle_clicked() {
                // a sequence tab closes its sequence; the Timeline panel itself stays
                match tab.seq.or(seqs.active.filter(|_| *p == PanelKind::Timeline)) {
                    Some(id) => actions.push(DockAction::CloseSequence(id)),
                    None => actions.push(DockAction::Close(*p)),
                }
            }
            x += w + 8.0;
        }
        // Dragging a sequence tab sideways: it changes places with a neighbour when the pointer
        // passes that neighbour's middle.
        if let Some(id) = dragged
            && let Some(px) = ui.input(|i| i.pointer.latest_pos()).map(|p| p.x)
            && let Some(at) = seq_rects.iter().position(|(s, _)| *s == id)
            && let Some(index) = seqs.open.iter().position(|(s, _)| *s == id)
        {
            let before = at.checked_sub(1).and_then(|k| seq_rects.get(k));
            let after = seq_rects.get(at + 1);
            if before.is_some_and(|(_, r)| px < r.center().x) {
                actions.push(DockAction::MoveSequence(id, index.saturating_sub(1)));
            } else if after.is_some_and(|(_, r)| px > r.center().x) {
                actions.push(DockAction::MoveSequence(id, index + 1));
            }
        }
        // the » list: every tab of the group, the shown one ticked
        let list_open = ui.ctx().data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false);
        if let Some(r) = more.filter(|_| list_open) {
            let mut picked = None;
            let area = egui::Area::new(list_id.with("area")).order(egui::Order::Foreground).pivot(Align2::RIGHT_TOP).fixed_pos(r.right_bottom()).show(
                ui.ctx(),
                |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(160.0);
                        // many open sequences: the list scrolls instead of leaving the window
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            for tab in &tabs {
                                let label = if tab.active { format!("✓ {}", tab.title) } else { tab.title.clone() };
                                let resp = ui.button(label);
                                match tab.seq {
                                    Some(id) => reg.add_response(&format!("timeline.tabs.list.{id}"), &resp, &tab.title),
                                    None => reg.add_response(&format!("panel.tabs.list.{}", tab.panel.id()), &resp, &tab.title),
                                }
                                if resp.clicked() {
                                    picked = Some(tab);
                                }
                            }
                        });
                    });
                },
            );
            if let Some(tab) = picked {
                activate(tab, &mut actions);
                actions.push(DockAction::Focus(tab.panel));
            }
            if picked.is_some() || (!list_opened && area.response.clicked_elsewhere()) {
                ui.ctx().data_mut(|d| d.remove::<bool>(list_id));
            }
        } else if list_open {
            ui.ctx().data_mut(|d| d.remove::<bool>(list_id));
        }
    }
    // focus outline
    if active_panel == Some(focused) {
        painter.rect_stroke(g.rect, 0.0, Stroke::new(1.0, t.focus), StrokeKind::Inside);
    }
    // clicking anywhere in the panel focuses it
    if let Some(p) = active_panel
        && ui.rect_contains_pointer(g.rect)
        && ui.input(|i| i.pointer.any_pressed())
    {
        actions.push(DockAction::Focus(p));
    }
    Chrome { actions, tabs: tab_rects }
}

fn paint_widget_focus(painter: &egui::Painter, response: &egui::Response, tokens: &Tokens) {
    if response.has_focus() {
        painter.rect_stroke(response.rect.shrink(2.0), 2.0, Stroke::new(1.5, tokens.focus), StrokeKind::Inside);
    }
}

/// Placeholder body for panels that are not implemented yet.
pub fn placeholder(ui: &mut egui::Ui, rect: Rect, t: &Tokens, text: &str) {
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, Tokens::ui(12.0), t.text_faint);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_first_never_panics_on_tiny_areas() {
        let split_first = |size: SplitSize, avail: f32| {
            split_rect(Rect::from_min_size(egui::Pos2::ZERO, vec2(avail, 100.0)), SplitAxis::Horizontal, size.shared(), 0.0, 20.0).unwrap().first_size()
        };
        for avail in [0.0, 1.0, 15.0, 20.0, 35.0, 39.9, 40.0, 100.0, 2000.0] {
            for size in [
                SplitSize::Ratio(0.0),
                SplitSize::Ratio(0.5),
                SplitSize::Ratio(1.0),
                SplitSize::FixedA(300.0),
                SplitSize::FixedB(300.0),
                SplitSize::FixedA(0.0),
            ] {
                let f = split_first(size, avail);
                assert!((0.0..=avail).contains(&f), "{size:?} in {avail}: {f}");
            }
        }
        assert_eq!(split_first(SplitSize::FixedA(300.0), 1000.0), 300.0);
        assert_eq!(split_first(SplitSize::FixedB(300.0), 1000.0), 700.0);
        assert_eq!(split_first(SplitSize::Ratio(0.0), 1000.0), 20.0);
    }

    #[test]
    fn workspaces_contain_core_panels() {
        for w in WORKSPACES {
            let d = workspace(w);
            assert!(d.contains(PanelKind::Timeline), "{w}");
            assert!(d.contains(PanelKind::Program), "{w}");
        }
    }

    /// Fork RS3: the Premiere workflow's built-in workspaces stay byte-for-byte what they were
    /// before the Resolve pages (FNV-1a of each layout's JSON, measured on the pre-RS3 tree).
    /// Color Grading is the fork's own (G1, shared with the Resolve Color page): re-measured when
    /// it gained the Curves panel (FORK-PLAN §1a.57).
    #[test]
    fn premiere_builtin_workspaces_are_unchanged() {
        fn fnv(s: &str) -> u64 {
            s.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
        }
        let got: Vec<(&str, u64)> = WORKSPACES.iter().map(|w| (*w, fnv(&serde_json::to_string(&workspace(w)).unwrap()))).collect();
        let want: [(&str, u64); 11] = [
            ("Editing", 0x2d97a8099673ed40),
            ("Assembly", 0xee28d0d99897cb70),
            ("Color", 0xca51e1b14eabdf32),
            ("Color Grading", 0x52bc1a0377904630),
            ("Effects", 0xbd17ffaeeb6d7ce9),
            ("Audio", 0x24656433eab1d7cf),
            ("Captions and Graphics", 0x83110857e008a2ee),
            ("Compositing", 0x59e002669a53c992),
            ("Learning", 0x2d97a8099673ed40),
            ("Review", 0x2d97a8099673ed40),
            ("All Panels", 0x2b7399c614e112fa),
        ];
        assert_eq!(got, want);
    }

    /// Fork RS3: the page workspaces are valid layouts, listed only in the Resolve workflow, and
    /// each workflow maps the other's names onto its own.
    #[test]
    fn page_workspaces_per_workflow() {
        for (w, premiere) in PAGE_WORKSPACES {
            let l = Layout { root: workspace(w), floating: vec![] };
            l.validate().unwrap_or_else(|e| panic!("{w}: {e}"));
            assert!(is_builtin(w));
            assert_eq!(for_workflow(w, false), premiere);
            assert_eq!(for_workflow(w, true), w);
        }
        assert_eq!(for_workflow("Editing", true), "Edit Page");
        assert_eq!(for_workflow("Color", true), "Color Grading");
        assert_eq!(for_workflow("Editing", false), "Editing");
        assert_eq!(for_workflow("My Own", true), "My Own");
        let mut prefs = WorkspacePrefs::default();
        prefs.saved.push(SavedWorkspace {
            name: "Edit Page".into(),
            layout: serde_json::to_value(workspace("Edit Page")).unwrap(),
            floating: serde_json::Value::Null,
        });
        prefs.saved.push(SavedWorkspace {
            name: "Mine".into(),
            layout: serde_json::to_value(workspace("Editing")).unwrap(),
            floating: serde_json::Value::Null,
        });
        let premiere = names(&prefs);
        let mut want: Vec<String> = WORKSPACES.iter().map(|s| s.to_string()).chain(["Mine".to_string()]).collect();
        want.sort_by_key(|n| n.to_lowercase());
        assert_eq!(premiere, want, "Premiere's list never shows a page");
        let resolve = names_for(&prefs, true);
        assert!(resolve.iter().any(|n| n == "Edit Page") && resolve.iter().any(|n| n == "Mine"));
        assert!(!resolve.iter().any(|n| n == "Editing" || n == "Assembly" || n == "Audio" || n == "Color"), "{resolve:?}");
        assert_eq!(find(&prefs, "editpage").as_deref(), Some("Edit Page"));
        assert_eq!(find(&prefs, "editing").as_deref(), Some("Editing"));
    }

    /// Fork RS3 review: a user's own pre-RS3 workspace named like a page stays reachable.
    #[test]
    fn pre_rs3_workspaces_named_like_pages_are_renamed_once() {
        let json =
            r#"{"saved":[{"name":"Edit Page","layout":null},{"name":"Edit Page (custom)","layout":null},{"name":"Mine","layout":null}],"current":"Edit Page"}"#;
        let dir = std::env::temp_dir().join(format!("fc-rs3-ws-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(WORKSPACES_FILE);
        std::fs::write(&path, json).unwrap();
        let p = WorkspacePrefs::load(&path);
        let names: Vec<&str> = p.saved.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Edit Page (custom 2)", "Edit Page (custom)", "Mine"]);
        assert_eq!(p.current, "Edit Page (custom 2)");
        assert_eq!(p.pages_version, 1);
        assert!(names_for(&p, false).iter().any(|n| n == "Edit Page (custom 2)"), "listed in Premiere");
        // saved again by an RS3 build: a saved change to the built-in Edit Page is kept as is
        let mut q = WorkspacePrefs::default();
        q.saved.push(SavedWorkspace { name: "Edit Page".into(), layout: serde_json::Value::Null, floating: serde_json::Value::Null });
        q.save(&path).unwrap();
        assert_eq!(WorkspacePrefs::load(&path).saved[0].name, "Edit Page");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_side_opens_a_column_once() {
        let mut d = workspace("Edit Page");
        d.add_side(PanelKind::EffectControls, false, 360.0);
        assert!(d.is_visible(PanelKind::EffectControls));
        assert!(
            matches!(&d, DockNode::Split { vertical: false, size: SplitSize::FixedB(w), b, .. } if *w == 360.0 && **b == tabs(&[PanelKind::EffectControls], 0))
        );
        let before = d.clone();
        d.add_side(PanelKind::EffectControls, true, 300.0);
        assert_eq!(d, before, "already there: nothing changes");
        Layout { root: d.clone(), floating: vec![] }.validate().unwrap();
        let mut empty = tabs(&[], 0);
        empty.add_side(PanelKind::Project, true, 380.0);
        assert_eq!(empty, tabs(&[PanelKind::Project], 0));
    }

    #[test]
    fn close_and_open() {
        let mut d = workspace("Editing");
        d.close(PanelKind::Metadata);
        assert!(!d.contains(PanelKind::Metadata));
        d.open_near(PanelKind::LumetriColor, PanelKind::Program);
        assert!(d.is_visible(PanelKind::LumetriColor));
        let s = serde_json::to_string(&d).unwrap();
        let back: DockNode = serde_json::from_str(&s).unwrap();
        assert_eq!(back, d);
    }

    /// #284: with Project and Program closed (or every panel closed) a panel still opens.
    #[test]
    fn open_near_falls_back_to_the_first_group() {
        let mut d = workspace("Editing");
        let mut all = Vec::new();
        d.panels(&mut all);
        for p in all {
            d.close(p);
        }
        assert_eq!(d, DockNode::Tabs { panels: vec![], active: 0 });
        d.open_near(PanelKind::Program, PanelKind::Project);
        assert!(d.is_visible(PanelKind::Program));
        d.restore_timeline();
        assert!(d.is_visible(PanelKind::Timeline));
        // a tree without Project or Program: the panel joins the first group
        let mut d = hsplit(SplitSize::Ratio(0.5), tabs(&[PanelKind::Effects], 0), tabs(&[PanelKind::Markers], 0));
        d.open_near(PanelKind::Info, PanelKind::Source);
        assert!(d.is_visible(PanelKind::Info));
        assert_eq!(d, hsplit(SplitSize::Ratio(0.5), tabs(&[PanelKind::Effects, PanelKind::Info], 1), tabs(&[PanelKind::Markers], 0)));
    }
}
