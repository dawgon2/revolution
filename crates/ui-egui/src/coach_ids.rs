//! Which automation ids exist, for "Ask the coach" (fork GRADE-PLAN G2.12): every control an
//! answer names is checked, and unknown ones are removed (`filmcraft_engine::coach_ask`).
//!
//! An id is known when the automation registry has shown it this session ([`SeenIds`], filled
//! each frame by [`crate::automation::Registry::begin_frame`]) or it matches one of the ids the
//! UI builds at run time ([`DYNAMIC_ID_PATTERNS`], one per `format!("…{…}…")` id in the source).
//! In a pattern, `*` stands for one or more characters within one dot-separated part, and a
//! part that is just `**` for one or more whole parts (`grade.param.**` covers
//! `grade.param.qual.soft`). A test keeps the list equal to the source's `format!` ids.
//!
//! Where the variable part has a small fixed set of values, that set decides instead of the
//! pattern ([`narrowed`]: grade parameters, wheels, curves, tabs, window shapes, qualifier views,
//! compare modes, pages), so a misspelt `grade.param.saturaton` is unknown.
//!
//! [`SeenIds`] only remembers ids no pattern covers (clip, track and node ids would otherwise
//! fill it).

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

use crate::panels::grade::{CURVES, TABS, WHEELS, WINDOW_SHAPES};

/// Most ids remembered (a session that shows more stops remembering new ones).
pub const MAX_SEEN: usize = 50_000;

/// The automation ids built with `format!` in the UI, as patterns (see the module docs).
#[rustfmt::skip]
pub const DYNAMIC_ID_PATTERNS: &[&str] = &[
    "about.*", "about.credits.header.*", "about.credits.names.*", "about.tab.*", "addTracks.*.amount", "addTracks.*.placement",
    "addTracks.*.placement.option.*", "addTracks.*.type", "addTracks.*.type.option.*", "align.*", "assistant.doThis.*", "assistant.lesson.*", "assistant.line.*.*", "assistant.noSteps.*", "assistant.rate.*.correction", "assistant.rate.*.down", "assistant.rate.*.share", "assistant.rate.*.up", "assistant.reference.*", "assistant.seen.*", "assistant.setup.*", "assistant.step.*.*", "assistant.turn.*", "audioGain.*", "audioGain.*.value",
    "audioMeters.channel.*", "clipMixer.*", "clips.card.*", "clips.split.*", "coach.ask.assistLine.*", "coach.ask.step.*", "coach.lessons.item.*", "comp.canvas.insertTemplate.**", "comp.canvas.deleteTemplate.**", "comp.param.*", "comp.canvas.breadcrumb.*", "comp.expose.*", "comp.wheel.*", "comp.curve.*", "comp.param.*.stopwatch", "comp.inspector.*.expr", "comp.inspector.*.expr.toggle", "comp.inspector.*.expr.error", "comp.mask.*", "comp.mask*.*", "comp.roto*.*", "comp.roto*.track.*", "comp.paint*.*", "comp.track.analyse.*", "comp.track.point*.x", "comp.track.point*.y", "comp.track.point*.delete", "comp.planar.corner*.x", "comp.planar.corner*.y", "compViewer.roto*.point*.*", "compViewer.roto*.point*", "compViewer.track.point*", "compViewer.track.pattern*", "compViewer.track.search*", "compViewer.planar.corner*", "compViewer.planar.region*", "colorDialog.par.*", "colorDialog.space.*", "colorDialog.working.*",
    "deleteTracks.*", "deleteTracks.*.option.*", "deleteTracks.*.option.empty", "deleteTracks.*.target", "display.*", "dock.gutter.*",
    "editCameras.enabled.*", "editCameras.name.*", "editCameras.thumb.*", "effectControls.*.*", "effectControls.*.*.eyedropper",
    "effectControls.*.*.graphs", "effectControls.*.*.keyframe.*", "effectControls.*.*.reset", "effectControls.*.*.stopwatch",
    "effectControls.*.graph.keyframe.*", "effectControls.*.graph.keyframe.*.x", "effectControls.*.graph.keyframe.*.y",
    "effectControls.*.mask*", "effectControls.*.mask.*", "effectControls.effect.*", "effectControls.effect.*.edit",
    "effectControls.effect.*.savePreset", "effectControls.position.graph.keyframe.*.y", "effectControls.text.*.*",
    "effectControls.transition.alignment.option.*", "effectControls.transition.param.*", "effectControls.transition.timeline.*",
    "effects.folder.*", "effects.item.*", "effects.lumetriPreset.*", "effects.preset.*", "effects.presetGrid.*", "effects.user.broken.*", "essentialGraphics.tab.*",
    "essentialSound.*", "essentialSound.*.*", "essentialSound.browse.*.*", "essentialSound.ducking.against.*", "essentialSound.section.*",
    "essentialSound.section.*.toggle", "essentialSound.tab.*", "essentialSound.take.*.use", "essentialSound.type.*", "events.filter.*", "events.row.*",
    "export.effects.*.opacity", "export.effects.*.position", "export.effects.*.size", "export.effects.*.text", "export.metadata.*",
    "export.queue.item.*", "export.queue.item.*.*", "export.section.*", "exportFrame.depth.*", "exportFrame.format.*",
    "exportTemplate.control.*", "find.match.*", "find.row.*.column", "find.row.*.operator", "find.row.*.text", "fxEditor.*.*",
    "fxEditor.*.close", "fxEditor.*.curve", "fxEditor.*.curve.*", "fxEditor.*.node.*", "fxEditor.*.param.*", "fxEditor.*.plot",
    "fxEditor.*.reset", "fxEditor.*.toggle.*", "gfxTemplates.control.*", "gfxTemplates.item.*", "grade.curve.*", "grade.hdr.*.*", "grade.hdr.wheel.*", "grade.hdr.wheel.*.reset",
    "grade.param.**", "grade.param.**.reset", "grade.qual.show.*", "grade.tab.*", "grade.wheel.*", "grade.wheel.*.master", "grade.wheel.*.reset", "grade.window.*",
    "grade.window.*.delete", "grade.window.*.invert", "grade.window.*.softness", "grade.window.*.track.*", "grade.window.add.*", "grade.warper.mode.*", "grade.warper.point.*",
    "graphics.*", "graphics.align.*", "graphics.arrange.*", "graphics.distribute.*", "graphics.layers.*", "graphics.layers.*.visible",
    "graphics.pin.*", "graphics.prop.*", "graphics.roll.*", "graphics.section.*", "graphics.textProperties.type.*", "graphics.time.*",
    "guides.manage.row.*", "header.*", "header.mode.*", "import.*", "import.link.*", "inspector.*.*", "inspector.file.*", "inspector.section.*", "inspector.tab.*", "labels.colors.*.color", "labels.colors.*.name",
    "linkMedia.*", "linkMedia.candidate.*", "linkMedia.match.*", "linkMedia.row.*", "lumetri.*", "lumetri.param.*", "lumetri.section.*",
    "lumetri.switch.*", "markers.filter.*", "markers.row.*", "mcam.audio.*", "mcam.cameraNames.*", "mediaBrowser.*",
    "mediaBrowser.columns.*", "mediaBrowser.entry.*", "mediaBrowser.fileTypes.*", "mediaBrowser.header.*", "mediaBrowser.menu.*",
    "mediaBrowser.tree.*", "mediaBrowser.tree.*.*", "mediaBrowser.tree.dir.*", "mediaBrowser.view.*", "menu.**", "metadata.field.*",
    "metadata.label.*", "metadata.section.*", "mixer.*", "mixer.showHide.*", "mixer.transport.*", "page.*", "pageBar.*", "pageToolbar.*", "panel.*", "panel.drag.*",
    "panel.float.*.move", "panel.float.*.resize", "panel.menu.*", "panel.menu.*.maximize", "panel.menu.*.undock", "panel.menu.Source.*",
    "panel.menu.Source.recent.*", "panel.tab.*", "panel.tabs.list.*", "panel.tabs.more.*", "pausedRes.*", "playbackRes.*", "pm.mode.*",
    "pm.preset.*", "pm.seq.*", "presetManager.favorite.*", "presetManager.item.*", "program.compare.*", "program.layer.*.*",
    "program.layer.*.*.anchor", "program.layer.*.*.corner.*", "program.layer.*.*.handle.*", "program.mask.*", "program.mask.*.*",
    "program.mask.vertex.*", "program.multicam.*", "program.multicam.angle.*", "program.split.*", "progress.cancel.*", "progress.job.*", "project.bins.*", "project.itemMenu.*",
    "project.itemMenu.kdeconnect.*", "project.menu.*", "project.menu.fontSize.*", "project.menu.restoreViewPreset.*", "project.menu.view.*", "project.searchBin.*",
    "project.tab.*", "project.tab.*.close", "projectBin.*", "projectBin.*.window", "projectDialog.*", "projectDialog.down.*",
    "projectDialog.field.*", "projectDialog.preset.*", "projectDialog.up.*", "projectSettings.*.*", "projectSettings.scratch.*",
    "projectSettings.scratch.*.browse", "projectSettings.scratch.*.same", "projectSettings.tab.*", "projects.crumb.*",
    "projects.folder.*", "projects.menu.*", "projects.other.*", "projects.sidebar.*", "projects.sidebar.remote.*", "projects.sort.*", "projects.tile.*", "projects.transfer.entry.**", "projects.transfer.location.*", "projects.transfer.receiver.*", "properties.*.*", "proxies.preset.*",
    "quickExport.preset.*", "recovery.item.*", "reference.display.*", "reference.scope.*", "savePreset.keyframes.*", "remote.*", "remote.entry.**", "remote.location.*", "scopes.menu.*",
    "scopes.menu.brightness.*", "scopes.menu.colorSpace.*", "scopes.menu.paradeType.*", "scopes.menu.preset.*", "scopes.menu.scale.*",
    "scopes.menu.targets.*", "scopes.menu.waveformType.*", "sequenceSettings.*", "sequenceSettings.channelFormat.option.*",
    "sequenceSettings.sampleRate.option.*", "sequenceSettings.tab.*", "sequenceSettings.timebase.option.*/*", "sequenceSettings.tracks.*",
    "sequenceSettings.videoDisplay.option.*", "sequenceSettings.workingSpace.option.*", "settings.**", "settings.**.**",
    "settings.**.browse", "settings.**.hex", "settings.category.**", "shortcuts.add.*", "shortcuts.cell.*", "shortcuts.context.*",
    "shortcuts.key.*", "shortcuts.mod.*", "shortcuts.mod.*.right", "shortcuts.preset.*", "shortcuts.row.*", "stills.item.*",
    "stills.mode.*", "text.captions.*.*", "text.captions.*.goto", "text.captions.*.row", "text.captions.*.text",
    "text.captions.style.align.*", "text.captions.style.anchor.*", "text.captions.style.font.option.*", "text.tab.*",
    "text.transcript.paragraph.*", "text.transcript.word.*", "timecode.row.*", "timecode.row.*.display.*", "timecode.row.*.mode.*",
    "timecode.row.*.remove", "timecode.row.*.source.*", "timeline.caption.*", "timeline.captionTrack.*", "timeline.captionTrack.*.enabled",
    "timeline.captionTrack.*.locked", "timeline.captionTrack.*.menu.*", "timeline.clip.*", "timeline.clip.*.copy", "timeline.clip.*.empty",
    "timeline.clip.*.volume", "timeline.clip.*.volume.kf.*", "timeline.clip.*.volume.kf.*.delete", "timeline.clipMenu.*",
    "timeline.clipMenu.multicam.camera.*", "timeline.editPointMenu.*", "timeline.marker.*", "timeline.markerMenu.*",
    "timeline.renderBar.*", "timeline.settings.thumbnails.*", "timeline.split.*", "timeline.tab.*", "timeline.tab.*.close",
    "timeline.tabs.list.*", "timeline.throughEdit.*", "timeline.toggle.*", "timeline.track.*.enabled", "timeline.track.*.keyframes",
    "timeline.track.*.keyframes.*", "timeline.track.*.keyframes.clip", "timeline.track.*.lane", "timeline.track.*.lane.kf.*",
    "timeline.track.*.fxDrop", "timeline.track.*.header", "timeline.track.*.sendTo", "timeline.track.*.sendTo.*", "timeline.track.*.locked", "timeline.track.*.meter", "timeline.track.*.muted", "timeline.track.*.solo", "timeline.track.*.sourcePatch",
    "timeline.track.*.syncLock", "timeline.track.*.target", "timeline.track.*.voiceover", "timeline.transition.*",
    "timeline.transition.*.in", "timeline.transition.*.out", "tnr.*", "tnr.*.keyframe.*", "tools.*", "tools.select.*", "view.*", "view.display.*",
    "view.magnification.*", "voiceover.input.*", "voiceover.source.*", "window.panel.*", "window.workspace.*", "workspaces.edit.row.*",
    // C2b mixer controls (panels/grade_mixer.rs): widget ids, and the parameter keys they edit
    "effectControls.track.fx.*", "grade.mixer.*.weight", "grade.mixer.*.share", "grade.mixer.*.mode", "grade.mixer.*.opacity", "grade.mixer.*.invert", "mix.gain.*", "layer.mode.*",
    "layer.opacity.*", "kmix.mode.*", "kmix.invert.*",
    // LAY.3 layout presets (panels/layout_presets.rs)
    "layoutPresets.row.*", "window.layoutPreset.load.*",
];

/// Whether `part` (no dots) matches a glob where `*` is one or more characters.
fn glob(pat: &str, part: &str) -> bool {
    let Some((head, rest)) = pat.split_once('*') else { return pat == part };
    let Some(after) = part.strip_prefix(head) else { return false };
    // `*` takes at least one character: try every split point (parts are short)
    after.char_indices().map(|(i, _)| i).chain([after.len()]).filter(|i| *i > 0).any(|i| after.get(i..).is_some_and(|tail| glob(rest, tail)))
}

fn parts_match(pat: &[&str], id: &[&str], depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match (pat.split_first(), id.split_first()) {
        (None, None) => true,
        (Some((&"**", prest)), Some(_)) => (1..=id.len()).any(|k| id.get(k..).is_some_and(|tail| parts_match(prest, tail, depth + 1))),
        (Some((p, prest)), Some((i, irest))) => glob(p, i) && parts_match(prest, irest, depth + 1),
        _ => false,
    }
}

/// Whether `id` matches the pattern (see the module docs).
pub fn pattern_matches(pattern: &str, id: &str) -> bool {
    if id.is_empty() || id.len() > 256 {
        return false;
    }
    let p: Vec<&str> = pattern.split('.').collect();
    let i: Vec<&str> = id.split('.').collect();
    if i.len() > 32 || i.iter().any(|x| x.is_empty()) {
        return false;
    }
    parts_match(&p, &i, 0)
}

/// For ids whose variable part has a small fixed set of values: whether `id` uses one of them
/// (None: not such an id, the patterns decide).
pub fn narrowed(id: &str) -> Option<bool> {
    if let Some(key) = id.strip_prefix("grade.param.") {
        let known = |k: &str| filmcraft_project::grade::node_param(k).is_some();
        return Some(known(key) || key.strip_suffix(".reset").is_some_and(known));
    }
    let rest = |prefix: &str| id.strip_prefix(prefix);
    if let Some(k) = rest("grade.wheel.") {
        let k = k.strip_suffix(".reset").or_else(|| k.strip_suffix(".master")).unwrap_or(k);
        return Some(WHEELS.iter().any(|w| w.0 == k));
    }
    if let Some(k) = rest("grade.hdr.") {
        // (panels/grade_hdr.rs: the HDR wheels and their fields)
        use crate::panels::grade_hdr::{HDR_FIELDS, HDR_WHEELS};
        let wheel = |z: &str| HDR_WHEELS.iter().any(|w| w.0 == z);
        if let Some(w) = k.strip_prefix("wheel.") {
            return Some(wheel(w.strip_suffix(".reset").unwrap_or(w)));
        }
        if k == "reset" || k == "graph" {
            return Some(true);
        }
        let Some((z, f)) = k.split_once('.') else { return Some(false) };
        let fields = if z == "global" { &HDR_FIELDS[..2] } else { &HDR_FIELDS[..] };
        return Some(wheel(z) && fields.iter().any(|x| x.0 == f));
    }
    if let Some(k) = rest("grade.curve.") {
        return Some(CURVES.iter().any(|c| c.0.trim_start_matches("curve.") == k));
    }
    if let Some(k) = rest("grade.tab.") {
        return Some(TABS.contains(&k));
    }
    if let Some(k) = rest("grade.window.add.") {
        return Some(WINDOW_SHAPES.contains(&k));
    }
    if let Some(k) = rest("grade.qual.show.") {
        return Some(["0", "1", "2"].contains(&k));
    }
    if let Some(k) = rest("stills.mode.") {
        // (panels/stills.rs: the compare modes Off / Wipe / Split)
        return Some(["off", "wipe", "split"].contains(&k));
    }
    if let Some(k) = rest("projects.sort.") {
        // (panels/home.rs: the Project Manager's sort keys and Reverse order)
        return Some(crate::panels::home::SORT_KEYS.contains(&k) || k == "descending");
    }
    if let Some(k) = rest("projects.menu.") {
        return Some(crate::panels::home::MENU_ITEMS.contains(&k));
    }
    if let Some(k) = rest("page.") {
        return Some(crate::panels::pages::PAGES.iter().any(|p| p.0 == k));
    }
    if let Some(k) = rest("pageBar.") {
        // (panels/pages.rs: the page bar's buttons)
        return Some(
            ["app", "home", "settings", "saveLayout", "saveLayout.reset", "saveLayout.original", "saveLayout.preset", "saveLayout.presets"].contains(&k),
        );
    }
    if let Some(k) = rest("pageToolbar.") {
        // (panels/page_toolbar.rs: the pages' toolbar toggles and the project name)
        return Some(k == "title" || crate::panels::page_toolbar::all_keys().iter().any(|x| x == k));
    }
    None
}

/// Whether `id` matches one of [`DYNAMIC_ID_PATTERNS`] (the patterns alone).
pub fn in_pattern_list(id: &str) -> bool {
    DYNAMIC_ID_PATTERNS.iter().any(|p| pattern_matches(p, id))
}

/// Whether `id` is one the UI builds at run time.
pub fn matches_dynamic(id: &str) -> bool {
    narrowed(id).unwrap_or_else(|| in_pattern_list(id))
}

#[derive(Default)]
struct Seen {
    /// Ids shown that no pattern covers.
    ids: HashSet<String>,
    /// Hashes of ids shown that a pattern covers (checked once, then skipped cheaply).
    dynamic: HashSet<u64>,
}

/// The automation ids shown so far this session (shared with the engine's check). Ids a pattern
/// covers aren't stored (the pattern already knows them).
#[derive(Clone, Default)]
pub struct SeenIds(Arc<Mutex<Seen>>);

fn hash_of(id: &str) -> u64 {
    // DefaultHasher::new() has fixed keys: the same id always hashes the same
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

impl SeenIds {
    /// Remember these ids (at most [`MAX_SEEN`] of each kind).
    pub fn record<'a>(&self, ids: impl Iterator<Item = &'a str>) {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        for id in ids {
            if seen.ids.contains(id) {
                continue;
            }
            let h = hash_of(id);
            if seen.dynamic.contains(&h) {
                continue;
            }
            if matches_dynamic(id) {
                if seen.dynamic.len() < MAX_SEEN {
                    seen.dynamic.insert(h);
                }
            } else if seen.ids.len() < MAX_SEEN {
                seen.ids.insert(id.to_string());
            }
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).ids.contains(id)
    }

    /// The ids stored (those no pattern covers).
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The check installed in the session (`Session::coach_ids`).
pub struct UiIds(pub SeenIds);

impl filmcraft_engine::coach_ask::UiIdCheck for UiIds {
    fn known_ui_id(&self, id: &str) -> bool {
        self.0.contains(id) || matches_dynamic(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_match_one_part_or_many() {
        assert!(pattern_matches("grade.window.*.softness", "grade.window.0.softness"));
        assert!(!pattern_matches("grade.window.*.softness", "grade.window.0.1.softness"));
        assert!(!pattern_matches("grade.window.*.softness", "grade.window..softness"));
        assert!(pattern_matches("grade.param.**", "grade.param.qual.soft"));
        assert!(pattern_matches("grade.param.**", "grade.param.saturation"));
        assert!(!pattern_matches("grade.param.**", "grade.param"));
        assert!(pattern_matches("effectControls.*.mask*", "effectControls.3.mask2"));
        assert!(!pattern_matches("effectControls.*.mask*", "effectControls.3.mask"));
        assert!(pattern_matches("sequenceSettings.timebase.option.*/*", "sequenceSettings.timebase.option.30000/1001"));
        assert!(!pattern_matches("grade.tab.*", "grade.tabs.x"));
        assert!(matches_dynamic("coach.ask.step.3"));
        assert!(!matches_dynamic("grade.flux.capacitor"));
        // fixed key sets decide over the patterns
        assert!(matches_dynamic("grade.param.saturation") && matches_dynamic("grade.param.qual.soft") && matches_dynamic("grade.param.contrast.reset"));
        assert!(!matches_dynamic("grade.param.saturaton") && in_pattern_list("grade.param.saturaton"));
        assert!(matches_dynamic("grade.wheel.gain") && !matches_dynamic("grade.wheel.gian"));
        assert!(matches_dynamic("grade.tab.qualifier") && !matches_dynamic("grade.tab.qualifer"));
        assert!(matches_dynamic("grade.curve.master") && !matches_dynamic("grade.curve.y"));
        assert!(matches_dynamic("grade.hdr.wheel.specular") && matches_dynamic("grade.hdr.wheel.global.reset") && !matches_dynamic("grade.hdr.wheel.mid"));
        assert!(matches_dynamic("grade.hdr.shadow.falloff") && matches_dynamic("grade.hdr.global.sat"));
        assert!(!matches_dynamic("grade.hdr.global.range") && !matches_dynamic("grade.hdr.light.hue") && !matches_dynamic("grade.hdr.x"));
        assert!(matches_dynamic("grade.window.add.linear") && !matches_dynamic("grade.window.add.star"));
        assert!(matches_dynamic("grade.qual.show.2") && !matches_dynamic("grade.qual.show.3"));
        assert!(matches_dynamic("stills.mode.wipe") && !matches_dynamic("stills.mode.fade"));
        assert!(matches_dynamic("page.color") && !matches_dynamic("page.colour"));
        assert!(matches_dynamic("projects.sort.modified") && matches_dynamic("projects.sort.descending") && !matches_dynamic("projects.sort.size"));
        assert!(matches_dynamic("projects.menu.trash") && !matches_dynamic("projects.menu.delete"));
        assert!(matches_dynamic("projects.tile.3") && matches_dynamic("projects.tile.new"));
        // hostile ids
        assert!(!matches_dynamic(&"a.".repeat(10_000)));
        assert!(!matches_dynamic(&format!("settings{}", ".x".repeat(40))), "too many parts");
        assert!(!pattern_matches("*", &"x".repeat(300)));
    }

    #[test]
    fn seen_ids_are_remembered_and_capped() {
        let s = SeenIds::default();
        s.record(["a.b", "a.b", "c.d"].into_iter());
        assert_eq!(s.len(), 2);
        assert!(s.contains("c.d") && !s.contains("e.f"));
        // ids a pattern covers aren't stored
        s.record(["timeline.clip.42", "clips.card.7"].into_iter());
        assert_eq!(s.len(), 2);
        let many: Vec<String> = (0..MAX_SEEN + 10).map(|i| format!("x.{i}")).collect();
        s.record(many.iter().map(String::as_str));
        assert_eq!(s.len(), MAX_SEEN);
    }

    /// Every `format!("ns.…{…}…")` id in the UI source is covered by a pattern, and every pattern
    /// still has its `format!` in the source.
    #[test]
    fn the_pattern_list_matches_the_source() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut formats: Vec<String> = Vec::new();
        let mut dirs = vec![root];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).expect("src is readable") {
                let p = e.expect("entry").path();
                if p.is_dir() {
                    dirs.push(p);
                    continue;
                }
                if p.extension().is_none_or(|x| x != "rs") || p.file_name().is_some_and(|n| n == "coach_ids.rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&p).expect("readable");
                for chunk in src.split("format!(\"").skip(1) {
                    let Some(s) = chunk.split('"').next() else { continue };
                    let ns_ok = s.split('.').next().is_some_and(|ns| !ns.is_empty() && ns.bytes().all(|b| b.is_ascii_alphabetic()));
                    if s.contains('{') && s.contains('.') && !s.contains(' ') && s.starts_with(|c: char| c.is_ascii_lowercase()) && ns_ok {
                        formats.push(s.to_string());
                    }
                }
            }
        }
        // an instance of a format: each `{…}` filled in with "x7"
        let instance = |f: &str| {
            let mut out = String::new();
            let mut inside = false;
            for c in f.chars() {
                match (c, inside) {
                    ('{', _) => inside = true,
                    ('}', true) => {
                        inside = false;
                        out.push_str("x7");
                    }
                    (_, true) => {}
                    (c, false) => out.push(c),
                }
            }
            out
        };
        let missing: Vec<&String> = formats.iter().filter(|f| !in_pattern_list(&instance(f))).collect();
        assert!(missing.is_empty(), "add these to DYNAMIC_ID_PATTERNS: {missing:?}");
        let stale: Vec<&&str> = DYNAMIC_ID_PATTERNS.iter().filter(|p| !formats.iter().any(|f| pattern_matches(p, &instance(f)))).collect();
        assert!(stale.is_empty(), "no `format!` builds these any more: {stale:?}");
    }
}
