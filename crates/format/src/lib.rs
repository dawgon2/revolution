//! `.fcproj` project files.
//!
//! A project file is UTF-8 JSON with an explicit schema version:
//!
//! ```json
//! { "format": "filmcraft.project", "schema_version": 2, "generator": "FilmCraft 0.1.0", "project": { … } }
//! ```
//!
//! - **Versioning.** [`SCHEMA_VERSION`] is the version this build writes. Older files are upgraded on
//!   load by a chain of single-step migrations ([`MIGRATIONS`]: v1→v2, v2→v3, …) operating on the
//!   JSON document, so every historical shape only needs one small function. Newer files are refused
//!   with [`FormatError::TooNew`] instead of being half-read (and later overwritten with data loss).
//! - **Atomic saves.** [`atomic_write`] writes a temp file in the destination directory, fsyncs it,
//!   renames it over the target and fsyncs the directory: a crash or power cut during a save leaves
//!   either the old file or the new one, never a torn mix.
//! - **Auto-save naming/rotation** lives in [`autosave`] (`<name>-YYYY-MM-DD_HH-MM-SS.fcproj` in an
//!   `Auto-Save` folder next to the project, oldest pruned beyond the version limit).
//!
//! Schema history:
//! - **v1** (M0–M11): the bare serialized `Project` object with a `"version": 1` field.
//! - **v2** (M11.1): the envelope above; `Project.version` dropped (the envelope carries it).
//! - **v3** (M10.2): graphic clips (`ItemKind::Graphic`, text/shape layers). The shape of older
//!   data is unchanged (no-op step); the bump makes builds without graphics refuse such files
//!   cleanly instead of failing to parse them.
//! - **v4** (M7.4): Essential Sound (clip audio types and settings, `EffectInstance::essential`).
//!   No-op step; older builds would otherwise drop these fields silently when saving.
//! - **v5** (M8.8): colour management (sequence working space, Interpret Footage colour space,
//!   project LUT library). No-op step, for the same reason.
//! - **v6** (M11.7/M11.9): media identity fingerprints and ingest settings. No-op step.
//! - **v7** (M5.5): effect masks (keyframable Bézier mask paths on effect instances). No-op step.
//! - **v8** (M12): multi-camera source sequences, multi-camera clips and merged clips. No-op step;
//!   older builds would drop the camera data when saving.
//! - **v12** (M10.7): graphics templates, rolls / crawls, responsive design (pins, intro / outro),
//!   per-character text styles (`TrackItem::graphic`, `EffectInstance::layer`) and source graphics
//!   (`Project::source_graphics`). No-op step; older builds would drop these fields when saving.
//! - **v13**: all container audio streams, and per-timeline-clip stream selection. Old single-stream
//!   `audio` objects read as stream zero; older builds must refuse new multi-stream projects.
//! - **v14** (M7.9): narrations (`Project::narrations`, Text to Speech). No-op step; older builds
//!   would drop them when saving.
//! - **v15**: project notes (`Project::notes`, Window ▸ Project Notes). No-op step; older builds
//!   would drop them when saving.
//! - **v16** (fork G0 / G1.5): colour grades (`TrackItem::grade`, serial correction nodes; fork
//!   ADR 0004) and the colour stills gallery (`Project::stills`). No-op step; older builds would
//!   drop both when saving.
//! - **v17** (fork 6.7b): clip-level temporal noise reduction (`TrackItem::temporal_nr`). The
//!   `tnr.*` keys of a clip's first enabled grade node (the only ones that rendered) move to the
//!   clip; `tnr.*` keys on every other node (ignored before) are dropped. The first step that
//!   changes data ([`v16_to_v17`]).
//! - **v18** (fork C1): compositions (`ItemKind::Comp`, a node graph as a project item; fork ADR
//!   0007). No-op step; older builds refuse a project that contains a comp (unknown item kind), so
//!   the bump makes them say "too new" instead.
//! - **v19** (fork C2): branched grades (`Grade::edges` / `Grade::output`, `GradeNode::kind`:
//!   parallel / layer mixers, splitter / combiner, key mixer; `docs/fork/DESIGN-C2.md`). No-op
//!   step (serial grades are unchanged and still store no edges); older builds would silently drop
//!   the connections and node kinds, so the bump makes them refuse the file instead. Also in v19
//!   (fork C2b, before release): optional node-canvas positions (`GradeNode::pos`,
//!   `Grade::input_pos`), written only once a node was dragged, so an undragged grade saves as
//!   before; older builds would only lose the layout.
//!
//! **Fork lineage.** Before the first upstream sync (U0) the fork numbered the grades v13 and the
//! stills v14, both no-op steps on an upstream v12 document, while upstream used v13 for
//! multi-stream audio, v14 for narrations and v15 for notes. A fork-written v13 / v14 file is
//! therefore an upstream v12 document plus `grade` / `stills` fields: [`decode`] recognises one
//! ([`fork_shape`]: the pre-v13 single `audio` object on a media item, a `stills` list or a
//! `grade` on a clip) and runs the chain from v12, which keeps those fields (they load by name).
//! Every file this fork writes carries `"lineage": "fork/hevc-vaapi"` ([`FORK_LINEAGE`]) in the
//! envelope so later collisions are decided by the marker: a file at this build's own schema
//! without it was written by another lineage and is refused rather than misread. Upstream builds
//! ignore the field (the envelope tolerates unknown keys both ways).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod atomic;
pub mod autosave;

use filmcraft_project::{Project, ProjectView};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use atomic::atomic_write;

/// The `format` tag written in every envelope.
pub const FORMAT_ID: &str = "filmcraft.project";

/// A migration upgrades a whole document from schema `n` to `n + 1`.
pub type Migration = fn(Value) -> Result<Value, String>;

/// `MIGRATIONS[i]` upgrades schema `i + 1` to `i + 2`. Append one function per schema bump; never
/// edit a shipped one.
pub const MIGRATIONS: &[Migration] = &[
    v1_to_v2, v2_to_v3, v3_to_v4, v4_to_v5, v5_to_v6, v6_to_v7, v7_to_v8, v8_to_v9, v9_to_v10, v10_to_v11, v11_to_v12, v12_to_v13, v13_to_v14, v14_to_v15,
    v15_to_v16, v16_to_v17, v17_to_v18, v18_to_v19,
];

/// The schema version this build writes (and the newest it reads).
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32 + 1;

/// The `lineage` tag written in every envelope by this fork (see the module docs).
pub const FORK_LINEAGE: &str = "fork/hevc-vaapi";

/// Schema numbers the fork used before U0 with its own meaning (grades, stills): a file at one of
/// them without the lineage marker is read as upstream v12 when [`fork_shape`] says it is ours.
const FORK_LEGACY_SCHEMAS: std::ops::RangeInclusive<u32> = 13..=14;

/// The upstream schema a pre-U0 fork file really has.
const FORK_LEGACY_BASE: u32 = 12;

/// Oldest schema this build can still upgrade.
pub const OLDEST_SCHEMA: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("not a Revolution project: {0}")]
    NotAProject(String),
    #[error(
        "this project was saved by a newer version of Revolution (project schema v{found}); this build reads up to v{supported}. Update Revolution to open it."
    )]
    TooNew { found: u32, supported: u32 },
    #[error("project schema v{found} is too old for this build (oldest supported: v{oldest})")]
    TooOld { found: u32, oldest: u32 },
    #[error("upgrading project from schema v{from} to v{}: {msg}", from + 1)]
    Migration { from: u32, msg: String },
    #[error("project file is damaged: {0}")]
    Corrupt(String),
    #[error(
        "this project (schema v{found}) was saved by FilmCraft or another build that isn't Revolution; its v{found} fields differ from this build's. Open it with the app that wrote it."
    )]
    OtherLineage { found: u32 },
}

/// A project read from disk.
#[derive(Debug)]
pub struct Loaded {
    pub project: Project,
    /// Schema version found in the file (before migration).
    pub schema_version: u32,
    /// The `generator` string of the writer, if recorded.
    pub generator: Option<String>,
    /// What was open when the file was saved (sequence tabs and how each was shown), when the
    /// file records it and it can be read.
    pub view: Option<ProjectView>,
    /// What the upgrade changed that the user should hear about (one plain sentence each);
    /// empty when nothing was moved or the file was already current.
    pub migration_notes: Vec<String>,
}

impl Loaded {
    /// True when the file was written in an older schema and upgraded on load.
    pub fn migrated(&self) -> bool {
        self.schema_version < SCHEMA_VERSION
    }
}

/// Writer identification stored in the envelope.
pub fn generator() -> String {
    format!("Revolution {}", env!("CARGO_PKG_VERSION"))
}

#[derive(Serialize)]
struct EnvelopeOut<'a> {
    format: &'static str,
    schema_version: u32,
    generator: String,
    /// [`FORK_LINEAGE`]: which FilmCraft lineage's schema numbering the file follows.
    lineage: &'static str,
    project: &'a Project,
    #[serde(skip_serializing_if = "Option::is_none")]
    view: Option<&'a ProjectView>,
}

#[derive(Deserialize)]
struct EnvelopeIn {
    #[serde(default)]
    generator: Option<String>,
    project: Project,
    /// Kept as JSON until the project itself has loaded: a view that cannot be read is dropped,
    /// it never stops a project from opening.
    #[serde(default)]
    view: Option<Value>,
    /// [`MIGRATION_NOTES`], left in the document by migration steps; never written.
    #[serde(default, rename = "migration_notes")]
    notes: Vec<String>,
}

/// Envelope key under which migration steps leave notes for [`Loaded::migration_notes`].
const MIGRATION_NOTES: &str = "migration_notes";

impl EnvelopeIn {
    fn loaded(self, schema_version: u32) -> Loaded {
        let view = self.view.and_then(|v| serde_json::from_value(v).ok());
        // notes only come from this build's migrations, never from a file that was already current
        let migration_notes = if schema_version < SCHEMA_VERSION { self.notes } else { Vec::new() };
        Loaded { project: self.project, schema_version, generator: self.generator, view, migration_notes }
    }
}

/// Just enough of a document to know which schema it is.
#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    schema_version: Option<u32>,
    /// [`FORK_LINEAGE`] on files this fork wrote after U0; absent on upstream's and on older fork files.
    #[serde(default)]
    lineage: Option<String>,
    /// Bare (pre-envelope) project: its `version` field.
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    items: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    root: Option<serde::de::IgnoredAny>,
}

/// Serialize a project in the current schema. `pretty` = indented (human-diffable; used for the
/// project file itself), compact otherwise (recovery snapshots).
pub fn encode(project: &Project, pretty: bool) -> Vec<u8> {
    encode_with_view(project, None, pretty)
}

/// [`encode`], with what is open in the editor (`view`) stored beside the project. The view is
/// outside the schema's promises: builds that do not know it ignore it, and it is dropped rather
/// than migrated when it cannot be read.
pub fn encode_with_view(project: &Project, view: Option<&ProjectView>, pretty: bool) -> Vec<u8> {
    let env = EnvelopeOut { format: FORMAT_ID, schema_version: SCHEMA_VERSION, generator: generator(), lineage: FORK_LINEAGE, project, view };
    let r = if pretty { serde_json::to_vec_pretty(&env) } else { serde_json::to_vec(&env) };
    // Serializing plain data (string keys, finite numbers or null) cannot fail.
    r.unwrap_or_default()
}

/// Detect the schema version of a parsed document.
pub fn detect_version(doc: &Value) -> Result<u32, FormatError> {
    let probe: Probe = serde_json::from_value(doc.clone()).map_err(|e| FormatError::NotAProject(e.to_string()))?;
    probe_version(&probe)
}

fn probe_version(p: &Probe) -> Result<u32, FormatError> {
    if let Some(v) = p.schema_version {
        if let Some(f) = &p.format
            && f != FORMAT_ID
        {
            return Err(FormatError::NotAProject(format!("format is `{f}`, expected `{FORMAT_ID}`")));
        }
        return Ok(v);
    }
    // Before the envelope (v1) a file was the bare Project object with `version`.
    // Its `version` was 1, or 2 on builds that briefly bumped it when caption tracks were added
    // (same shape; the new field loads with its default). Anything higher is from the future.
    if p.items.is_some() && p.root.is_some() {
        return Ok(match p.version.unwrap_or(1) {
            0..=2 => 1,
            v => v,
        });
    }
    Err(FormatError::NotAProject("no `schema_version` and not a legacy project".into()))
}

/// Read a project file (any supported schema).
pub fn decode(bytes: &[u8]) -> Result<Loaded, FormatError> {
    let probe: Probe = serde_json::from_slice(bytes)
        .map_err(|e| if e.is_syntax() || e.is_eof() { FormatError::Corrupt(e.to_string()) } else { FormatError::NotAProject(e.to_string()) })?;
    let found = probe_version(&probe)?;
    check_supported(found)?;
    let ours = probe.lineage.as_deref() == Some(FORK_LINEAGE);
    if found == SCHEMA_VERSION {
        if !ours {
            // the fork's newest number without its marker: another lineage's file of the same number
            return Err(FormatError::OtherLineage { found });
        }
        // Fast path: deserialize straight into the model.
        let env: EnvelopeIn = serde_json::from_slice(bytes).map_err(|e| FormatError::Corrupt(e.to_string()))?;
        return validate_loaded(env.loaded(found));
    }
    let mut doc: Value = serde_json::from_slice(bytes).map_err(|e| FormatError::Corrupt(e.to_string()))?;
    // notes come only from this build's steps, never from the file
    if let Some(env) = doc.as_object_mut() {
        env.remove(MIGRATION_NOTES);
    }
    // a pre-U0 fork file (grades / stills numbered 13 / 14) is an upstream v12 document plus fields
    let from = if !ours && FORK_LEGACY_SCHEMAS.contains(&found) && fork_shape(&doc) { FORK_LEGACY_BASE } else { found };
    let doc = migrate_with(MIGRATIONS, doc, from)?;
    let env: EnvelopeIn = serde_json::from_value(doc).map_err(|e| FormatError::Corrupt(format!("after upgrading from schema v{found}: {e}")))?;
    validate_loaded(env.loaded(found))
}

/// Whether an unmarked v13 / v14 document was written by this fork before U0 (see the module
/// docs): a media item still holding the pre-v13 single `audio` object (upstream's v13 writes
/// `audio_streams`), a `stills` list, or a `grade` on a timeline clip. A document with none of
/// these has the same shape in both lineages, so either reading is right.
pub fn fork_shape(doc: &Value) -> bool {
    let Some(project) = doc.get("project").and_then(Value::as_object) else { return false };
    if project.contains_key("stills") {
        return true;
    }
    let Some(items) = project.get("items").and_then(Value::as_object) else { return false };
    items.values().any(|item| {
        let kind = item.get("kind");
        if kind.and_then(|k| k.get("Media")).and_then(|m| m.get("info")).and_then(Value::as_object).is_some_and(|info| info.contains_key("audio")) {
            return true;
        }
        let Some(seq) = kind.and_then(|k| k.get("Sequence")) else { return false };
        ["video_tracks", "audio_tracks"].into_iter().any(|tracks| {
            seq.get(tracks).and_then(Value::as_array).is_some_and(|tracks| {
                tracks.iter().any(|t| t.get("items").and_then(Value::as_array).is_some_and(|clips| clips.iter().any(|c| c.get("grade").is_some())))
            })
        })
    })
}

/// Refuse only values that would overflow or exhaust memory later (`Sequence::check_bounds`);
/// structural rules such as overlaps are left to the editor, so projects that open today still open.
fn validate_loaded(mut loaded: Loaded) -> Result<Loaded, FormatError> {
    // Out-of-range audio stream data is repaired, not refused: failing the load would lose the
    // whole project over one bad index. A media item keeps its first `MAX_AUDIO_STREAMS` streams; a
    // clip pointing past that plays stream 0.
    let max = filmcraft_media::MAX_AUDIO_STREAMS;
    for item in loaded.project.items.values_mut() {
        let name = item.name.clone();
        match &mut item.kind {
            filmcraft_project::ItemKind::Media(m) if m.info.audio_streams.len() > max => {
                log::warn!("media `{name}`: {} audio streams, keeping the first {max}", m.info.audio_streams.len());
                m.info.audio_streams.truncate(max);
            }
            filmcraft_project::ItemKind::Sequence(sequence) => {
                let sequence = std::sync::Arc::make_mut(sequence);
                for clip in sequence.audio_tracks.iter_mut().flat_map(|t| t.items.iter_mut()) {
                    if clip.audio_stream >= max {
                        log::warn!("sequence `{name}`: clip `{}` uses audio stream {}, reset to 0", clip.name, clip.audio_stream);
                        clip.audio_stream = 0;
                    }
                }
            }
            // a comp from a file is brought within its caps (sizes, counts, texts); a cycle stays
            // for the renderer / Compositor panel to report
            filmcraft_project::ItemKind::Comp(comp) => std::sync::Arc::make_mut(comp).sanitize(),
            _ => {}
        }
    }
    for item in loaded.project.sequences() {
        if let filmcraft_project::ItemKind::Sequence(sequence) = &item.kind {
            sequence.check_bounds().map_err(|e| FormatError::Corrupt(format!("sequence `{}`: {e}", item.name)))?;
        }
    }
    Ok(loaded)
}

fn check_supported(found: u32) -> Result<(), FormatError> {
    if found > SCHEMA_VERSION {
        return Err(FormatError::TooNew { found, supported: SCHEMA_VERSION });
    }
    if found < OLDEST_SCHEMA {
        return Err(FormatError::TooOld { found, oldest: OLDEST_SCHEMA });
    }
    Ok(())
}

/// Run `table` from schema `from` up to `table.len() + 1`, stamping `schema_version` after each step.
pub fn migrate_with(table: &[Migration], mut doc: Value, from: u32) -> Result<Value, FormatError> {
    let target = table.len() as u32 + 1;
    if from > target {
        return Err(FormatError::TooNew { found: from, supported: target });
    }
    if from == 0 {
        return Err(FormatError::TooOld { found: 0, oldest: 1 });
    }
    for v in from..target {
        let step = table[(v - 1) as usize];
        doc = step(doc).map_err(|msg| FormatError::Migration { from: v, msg })?;
        match doc.as_object_mut() {
            Some(o) => {
                o.insert("schema_version".into(), Value::from(v + 1));
            }
            None => return Err(FormatError::Migration { from: v, msg: "migration produced a non-object document".into() }),
        }
    }
    Ok(doc)
}

/// v1 → v2: wrap the bare project in the envelope and drop the per-project `version` field.
fn v1_to_v2(doc: Value) -> Result<Value, String> {
    let Value::Object(mut project) = doc else { return Err("expected a JSON object".into()) };
    project.remove("version");
    Ok(serde_json::json!({
        "format": FORMAT_ID,
        "schema_version": 2,
        "generator": "Revolution (schema v1)",
        "project": Value::Object(project),
    }))
}

/// v2 → v3: graphic clips added; existing data needs no change.
fn v2_to_v3(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v3 → v4: Essential Sound fields added; existing data needs no change.
fn v3_to_v4(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v4 → v5: colour-management fields added; existing data needs no change.
fn v4_to_v5(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v5 → v6: media identity and ingest settings added; existing data needs no change.
fn v5_to_v6(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v6 → v7: effect masks (Bézier mask paths as keyframable parameters); existing data needs no change.
fn v6_to_v7(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v7 → v8: multi-camera source sequences / clips and merged clips added; existing data needs no
/// change.
fn v7_to_v8(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v8 → v9: per-media-item transcripts (`project.transcripts`) added; existing data needs no change.
fn v8_to_v9(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v9 → v10: clip time interpolation, Hold Filters, Field Options and audio source channels; the
/// Modify ▸ Audio Channels map; subclips' Restrict Trims flag. Existing data needs no change.
fn v9_to_v10(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v10 → v11: search bins (`project.search_bins`), Flash Cue markers, Project Settings safe areas,
/// capture format and scratch disks. Existing data needs no change.
fn v10_to_v11(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v11 → v12: graphics templates, rolls / crawls, responsive design and per-character text styles
/// on graphic clips, and source graphics. Existing data needs no change.
fn v11_to_v12(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v12 → v13: container audio streams and per-clip stream selection. `MediaInfo` reads the old
/// `audio` field as stream zero; `TrackItem::audio_stream` defaults to zero.
fn v12_to_v13(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v13 → v14: narrations (Text to Speech). Existing data needs no change.
fn v13_to_v14(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v14 → v15: project notes. Existing data needs no change.
fn v14_to_v15(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// v15 → v16 (fork): colour grades on clips and the stills gallery. Existing data needs no change.
fn v15_to_v16(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

fn v17_to_v18(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

fn v18_to_v19(doc: Value) -> Result<Value, String> {
    Ok(doc)
}

/// The v16 grade-node keys that move to the clip in v17, with their clip key.
const TNR_NODE_KEYS: [(&str, &str); 4] = [("tnr.luma", "luma"), ("tnr.chroma", "chroma"), ("tnr.frames", "frames"), ("tnr.motion", "motion")];

/// v16 → v17 (fork 6.7b, `docs/fork/DESIGN-6.7b.md` §1.2): temporal NR moves from the grade to the
/// clip. On every video clip of every sequence, the `tnr.*` values of the grade's first enabled
/// node (what rendered in v16) become `temporal_nr` (switched off when the grade was bypassed,
/// which rendered no TNR), and every node loses its `tnr.*` keys (the other nodes' were ignored).
/// A clip that already has `temporal_nr` keeps it. Works on the JSON so shapes this build can't
/// type still migrate; anything unexpected is left as it is (the typed load judges it after).
fn v16_to_v17(mut doc: Value) -> Result<Value, String> {
    let (mut moved, mut dropped) = (0usize, 0usize);
    // project items a clip's temporal NR can sit on (media or subclip; the renderer narrows it
    // further); clips of anything else (nested sequences, graphics…) rendered none, so their
    // node keys are dropped like any ignored ones
    let media: std::collections::HashSet<String> = doc
        .get("project")
        .and_then(|p| p.get("items"))
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter(|(_, it)| it.get("kind").and_then(Value::as_object).is_some_and(|k| k.contains_key("Media") || k.contains_key("Subclip")))
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    // stills keep grades too: their `tnr.*` keys would come back on a node when applied
    if let Some(stills) = doc.get_mut("project").and_then(|p| p.get_mut("stills")).and_then(Value::as_array_mut) {
        for still in stills.iter_mut() {
            let Some(nodes) = still.get_mut("grade").and_then(|g| g.get_mut("nodes")).and_then(Value::as_array_mut) else { continue };
            dropped = dropped.saturating_add(strip_node_tnr(nodes, None).1);
        }
    }
    if let Some(items) = doc.get_mut("project").and_then(|p| p.get_mut("items")).and_then(Value::as_object_mut) {
        for item in items.values_mut() {
            let Some(tracks) = item.get_mut("kind").and_then(|k| k.get_mut("Sequence")).and_then(|s| s.get_mut("video_tracks")).and_then(Value::as_array_mut)
            else {
                continue;
            };
            for track in tracks.iter_mut() {
                let Some(clips) = track.get_mut("items").and_then(Value::as_array_mut) else { continue };
                for clip in clips.iter_mut() {
                    let takes = clip.get("item").and_then(Value::as_u64).is_some_and(|id| media.contains(&id.to_string()));
                    let (m, d) = move_clip_tnr(clip, takes);
                    moved = moved.saturating_add(usize::from(m));
                    dropped = dropped.saturating_add(d);
                }
            }
        }
    }
    if (moved > 0 || dropped > 0)
        && let Some(env) = doc.as_object_mut()
    {
        let note = format!("Temporal NR moved from the grade to the clip on {moved} clip(s); {dropped} ignored node setting(s) dropped.");
        // append: later steps may add their own notes
        match env.get_mut(MIGRATION_NOTES).and_then(Value::as_array_mut) {
            Some(notes) => notes.push(Value::String(note)),
            None => {
                env.insert(MIGRATION_NOTES.into(), Value::Array(vec![Value::String(note)]));
            }
        }
    }
    Ok(doc)
}

/// [`v16_to_v17`] for one clip (`takes`: its item can carry temporal NR): whether a clip setting
/// was written, and how many node keys that never rendered were dropped.
fn move_clip_tnr(clip: &mut Value, takes: bool) -> (bool, usize) {
    let Some(clip) = clip.as_object_mut() else { return (false, 0) };
    let Some(grade) = clip.get_mut("grade").and_then(Value::as_object_mut) else { return (false, 0) };
    let bypass = grade.get("bypass").and_then(Value::as_bool).unwrap_or(false);
    let Some(nodes) = grade.get_mut("nodes").and_then(Value::as_array_mut) else { return (false, 0) };
    // `Grade::active_nodes`: the first `MAX_NODES` nodes are the only ones that ever rendered
    let first_enabled = if takes {
        nodes.iter().take(filmcraft_project::grade::MAX_NODES).position(|n| n.is_object() && n.get("enabled").and_then(Value::as_bool).unwrap_or(true))
    } else {
        None
    };
    let (taken, dropped) = strip_node_tnr(nodes, first_enabled);
    if taken.is_empty() {
        return (false, dropped);
    }
    if clip.contains_key("temporal_nr") {
        // already a clip setting (hand-edited or written by a newer build): keep it
        return (false, dropped.saturating_add(taken.len()));
    }
    let mut tnr = serde_json::Map::new();
    if bypass {
        tnr.insert("enabled".into(), Value::Bool(false));
    }
    tnr.insert("params".into(), Value::Object(taken));
    clip.insert("temporal_nr".into(), Value::Object(tnr));
    (true, dropped)
}

/// Remove every `tnr.*` key from `nodes`: node `keep`'s values (an index) come back as the clip's
/// params (prefix stripped), the others are counted as dropped.
fn strip_node_tnr(nodes: &mut [Value], keep: Option<usize>) -> (serde_json::Map<String, Value>, usize) {
    let mut taken = serde_json::Map::new();
    let mut dropped = 0usize;
    for (i, node) in nodes.iter_mut().enumerate() {
        let Some(params) = node.get_mut("params").and_then(Value::as_object_mut) else { continue };
        let before = params.len();
        for (node_key, clip_key) in TNR_NODE_KEYS {
            let Some(v) = params.remove(node_key) else { continue };
            if Some(i) == keep {
                taken.insert(clip_key.into(), v);
            } else {
                dropped = dropped.saturating_add(1);
            }
        }
        if params.is_empty()
            && before > 0
            && let Some(n) = node.as_object_mut()
        {
            n.remove("params");
        }
    }
    (taken, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_matches_table() {
        assert_eq!(SCHEMA_VERSION, 19);
    }

    /// A project with a media item (one audio stream), a graded clip and a still, as the fork
    /// writes it today.
    fn graded_project() -> (Project, filmcraft_project::ItemId) {
        use filmcraft_media::{DemoScene, Generator, MediaSource, generators::GeneratorSource};
        use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, Still, TrackKind, grade::Grade};
        use filmcraft_time::{Tick, TimeRange};
        let mut p = Project::new("Fork Lineage");
        let src = GeneratorSource::demo(DemoScene::OceanSunset);
        let clip = MediaClip {
            media: MediaRef::Generator(Generator::Demo(DemoScene::OceanSunset)),
            info: src.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        let media = p.add_item("Ocean_Sunset.mp4", Label::Iris, ItemKind::Media(clip), None);
        let seq = p.new_sequence("Sequence 01", SequenceSettings::default(), 1, 1, None);
        let rate = p.sequence(seq).unwrap().settings.frame_rate;
        let range = TimeRange::new(Tick::ZERO, rate.tick_of(24));
        let mut ti = p.make_track_item(media, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
        // resolve the "centre" (NaN) points as an edit does, so the project compares equal to itself
        for e in &mut ti.effects {
            filmcraft_project::resolve_auto_points(e, (1920, 1080), (1920, 1080));
        }
        ti.grade = Some(Grade::with_node());
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        p.stills.push(Still {
            id: 1,
            label: "Hero".into(),
            clip: None,
            clip_name: "A001".into(),
            sequence: Some(seq),
            time: Tick(5),
            grade: Some(Grade::with_node()),
            width: 64,
            height: 36,
            nonce: 1,
        });
        (p, seq)
    }

    /// `encode(p)` rewritten as a pre-U0 fork file: schema `v`, no lineage marker, and every media
    /// item's `audio_streams` list back to the single `audio` object upstream v12 wrote.
    fn as_fork_legacy(p: &Project, v: u32) -> Vec<u8> {
        let mut doc: Value = serde_json::from_slice(&encode(p, false)).unwrap();
        doc.as_object_mut().unwrap().remove("lineage");
        doc["schema_version"] = Value::from(v);
        for item in doc["project"]["items"].as_object_mut().unwrap().values_mut() {
            if let Some(info) = item.pointer_mut("/kind/Media/info").and_then(Value::as_object_mut) {
                let first = info.remove("audio_streams").and_then(|s| s.as_array().and_then(|a| a.first().cloned())).unwrap_or(Value::Null);
                info.insert("audio".into(), first);
            }
        }
        serde_json::to_vec(&doc).unwrap()
    }

    fn media_streams(p: &Project) -> usize {
        p.items.values().find_map(|i| i.as_media()).map_or(0, |m| m.info.audio_streams.len())
    }

    /// Fork files from before U0 (grades at v13, stills at v14, upstream v12 audio) open: the
    /// audio is upgraded to a stream list, the grade and the still survive, and the file is
    /// written back at the current schema with the lineage marker.
    #[test]
    fn pre_u0_fork_v13_and_v14_files_load_with_grades_stills_and_upgraded_audio() {
        let (p, seq) = graded_project();
        assert_eq!(media_streams(&p), 1);
        for v in [13, 14] {
            let bytes = as_fork_legacy(&p, v);
            let doc: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(fork_shape(&doc), "v{v}: the single `audio` object marks a fork file");
            let text = String::from_utf8_lossy(&bytes).into_owned();
            assert!(text.contains("\"audio\":{") && !text.contains("audio_streams") && !text.contains("lineage"));
            let l = decode(&bytes).unwrap_or_else(|e| panic!("fork v{v} file refused: {e}"));
            assert_eq!((l.schema_version, l.migrated()), (v, true));
            assert_eq!(media_streams(&l.project), 1, "the old `audio` object is stream zero");
            assert_eq!(l.project, p, "grade and still intact, nothing else changed");
            let clip = &l.project.sequence(seq).unwrap().video_tracks[0].items[0];
            assert_eq!(clip.grade.as_ref().map(|g| g.nodes.len()), Some(1));
            assert_eq!(l.project.stills.len(), 1);
            let again: Value = serde_json::from_slice(&encode(&l.project, false)).unwrap();
            assert_eq!((again["schema_version"].as_u64(), again["lineage"].as_str()), (Some(u64::from(SCHEMA_VERSION)), Some(FORK_LINEAGE)));
        }
        // a fork v14 file without media still says so through its stills / grade
        let mut p2 = Project::new("Stills only");
        p2.stills = p.stills.clone();
        let doc: Value = serde_json::from_slice(&as_fork_legacy(&p2, 14)).unwrap();
        assert!(fork_shape(&doc));
        let mut doc: Value = serde_json::from_slice(&as_fork_legacy(&Project::new("Empty"), 13)).unwrap();
        assert!(!fork_shape(&doc), "nothing to tell the lineages apart: read as upstream, which is the same shape");
        assert!(decode(&serde_json::to_vec(&doc).unwrap()).is_ok());
        doc["project"] = Value::Null;
        assert!(!fork_shape(&doc), "a damaged document is not a fork file (and never a panic)");
    }

    /// Upstream's own v13 (multi-stream audio), v14 (narrations) and v15 (notes) files open and
    /// are written back at this fork's schema.
    #[test]
    fn upstream_v13_to_v15_files_load() {
        let (p, _) = graded_project();
        let mut plain = p.clone();
        plain.stills.clear();
        for item in plain.items.values_mut() {
            if let filmcraft_project::ItemKind::Sequence(seq) = &mut item.kind {
                for clip in std::sync::Arc::make_mut(seq).video_tracks.iter_mut().flat_map(|t| t.items.iter_mut()) {
                    clip.grade = None;
                }
            }
        }
        plain.notes = "upstream notes".into();
        for v in [13, 14, 15] {
            let mut doc: Value = serde_json::from_slice(&encode(&plain, false)).unwrap();
            doc.as_object_mut().unwrap().remove("lineage");
            doc["schema_version"] = Value::from(v);
            assert!(!fork_shape(&doc), "v{v}: upstream's `audio_streams`, no grade, no stills");
            let l = decode(&serde_json::to_vec(&doc).unwrap()).unwrap_or_else(|e| panic!("upstream v{v} file refused: {e}"));
            assert_eq!((l.schema_version, l.migrated()), (v, true));
            assert_eq!(l.project, plain);
            assert_eq!(media_streams(&l.project), 1);
            let again = decode(&encode(&l.project, true)).unwrap();
            assert_eq!((again.schema_version, &again.project), (SCHEMA_VERSION, &plain));
        }
    }

    /// The envelope carries the lineage marker; a file at this build's schema number without it
    /// comes from another lineage and is refused, never misread.
    #[test]
    fn lineage_marker_is_written_and_decides_same_numbered_files() {
        let p = Project::new("Marked");
        let mut doc: Value = serde_json::from_slice(&encode(&p, true)).unwrap();
        assert_eq!(doc["lineage"], FORK_LINEAGE);
        assert_eq!(decode(&serde_json::to_vec(&doc).unwrap()).unwrap().project, p);
        doc.as_object_mut().unwrap().remove("lineage");
        let e = decode(&serde_json::to_vec(&doc).unwrap()).unwrap_err();
        assert!(matches!(e, FormatError::OtherLineage { found } if found == SCHEMA_VERSION), "{e}");
        assert!(e.to_string().contains("isn't Revolution"), "{e}");
        // an unknown marker is not ours either; an older unmarked file (upstream's) still opens
        doc["lineage"] = Value::from("someone-else");
        assert!(matches!(decode(&serde_json::to_vec(&doc).unwrap()), Err(FormatError::OtherLineage { .. })));
        doc.as_object_mut().unwrap().remove("lineage");
        doc["schema_version"] = Value::from(SCHEMA_VERSION - 1);
        assert_eq!(decode(&serde_json::to_vec(&doc).unwrap()).unwrap().project, p);
    }

    #[test]
    fn damaged_sequence_settings_are_rejected_before_rendering() {
        let mut p = Project::new("Invalid sequence");
        let id = p.new_sequence("Bad", filmcraft_project::SequenceSettings::default(), 1, 1, None);
        for (width, height, sample_rate) in [(0, 90, 48000), (160, 0, 48000), (160, 90, 0), (65536, 90, 48000), (32768, 16384, 48000)] {
            let settings = &mut p.sequence_mut(id).unwrap().settings;
            (settings.width, settings.height, settings.sample_rate) = (width, height, sample_rate);
            assert!(matches!(decode(&encode(&p, false)), Err(FormatError::Corrupt(_))));
        }
    }

    /// #210 review: load-time validation refuses only overflowing values. A project that breaks a
    /// structural rule an older build may have written (overlapping or zero-length clips) still
    /// opens, as it did before; and 16K sequences are not "too large".
    #[test]
    fn structural_rule_breaks_still_open() {
        use filmcraft_project::{ItemKind, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, Tick, TimeRange};
        let mut p = Project::new("Legacy");
        let rate = FrameRate::FPS_24;
        let layer = p.add_item(
            "Layer",
            filmcraft_project::Label::Iris,
            ItemKind::AdjustmentLayer { width: 1920, height: 1080, rate, duration: rate.tick_of(48) },
            None,
        );
        let settings = SequenceSettings { width: 15360, height: 8640, ..Default::default() };
        let seq = p.new_sequence("Sequence 01", settings, 1, 1, None);
        let range = TimeRange::new(Tick::ZERO, rate.tick_of(48));
        let a = p.make_track_item(layer, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
        let b = p.make_track_item(layer, TrackKind::Video, rate.tick_of(24), range, rate).unwrap();
        let mut c = p.make_track_item(layer, TrackKind::Video, rate.tick_of(200), range, rate).unwrap();
        c.duration = Tick::ZERO;
        p.sequence_mut(seq).unwrap().video_tracks[0].items = vec![a, b, c];
        assert!(p.sequence(seq).unwrap().check().is_err(), "the sequence breaks the editor's invariants");
        let loaded = decode(&encode(&p, false)).unwrap_or_else(|e| panic!("a project that opened before is refused: {e}"));
        assert_eq!(loaded.project.sequence(seq).unwrap().video_tracks[0].items.len(), 3);
    }

    #[test]
    fn roundtrip_current() {
        let p = Project::new("Round Trip");
        for pretty in [true, false] {
            let b = encode(&p, pretty);
            let l = decode(&b).unwrap();
            assert_eq!(l.project, p);
            assert_eq!(l.schema_version, SCHEMA_VERSION);
            assert!(!l.migrated());
            assert_eq!(l.generator.as_deref(), Some(generator().as_str()));
        }
    }

    /// What is open is stored beside the project. A file without it, or with one that cannot be
    /// read, opens all the same.
    #[test]
    fn the_view_is_kept_beside_the_project_and_never_stops_a_file_from_opening() {
        use filmcraft_project::{ItemId, SequenceSettings, SequenceView};
        let mut p = Project::new("Tabs");
        let seq = p.new_sequence("Sequence 01", SequenceSettings::default(), 1, 1, None);
        let view = ProjectView {
            open_sequences: vec![seq],
            active_sequence: Some(seq),
            sequences: [(seq, SequenceView { pps: 80.0, scroll: 1.5, v_scroll: 0.0, a_scroll: 4.0, video_track_h: 60.0, audio_track_h: 56.0 })].into(),
            playheads: [(seq, filmcraft_time::Tick(3 * filmcraft_time::TICKS_PER_SECOND))].into(),
        };
        for pretty in [true, false] {
            let l = decode(&encode_with_view(&p, Some(&view), pretty)).unwrap();
            assert_eq!((l.view.as_ref(), &l.project), (Some(&view), &p));
            assert_eq!(decode(&encode(&p, pretty)).unwrap().view, None);
        }
        // the project is the same bytes with or without a view beside it
        let mut doc: Value = serde_json::from_slice(&encode_with_view(&p, Some(&view), false)).unwrap();
        assert_eq!(doc["project"], serde_json::from_slice::<Value>(&encode(&p, false)).unwrap()["project"]);
        // a view that is not one: dropped, the project loads
        for junk in [serde_json::json!(7), serde_json::json!({"open_sequences": {"a": 1}}), serde_json::json!({"sequences": [1]}), Value::Null] {
            doc["view"] = junk;
            let l = decode(&serde_json::to_vec(&doc).unwrap()).unwrap();
            assert_eq!((l.view, &l.project), (None, &p));
        }
        // unknown ids are not this crate's business (the editor checks them against the project)
        doc["view"] = serde_json::json!({"open_sequences": [424242]});
        assert_eq!(decode(&serde_json::to_vec(&doc).unwrap()).unwrap().view.unwrap().open_sequences, [ItemId(424242)]);
    }

    /// A clip with unresolved "auto" points (NaN) is written with `null` coordinates. Such a file
    /// used to be refused as damaged; it opens, and the points are still "auto".
    #[test]
    fn auto_points_written_as_null_open_again() {
        use filmcraft_project::{ItemKind, SequenceSettings, TrackKind};
        use filmcraft_time::{FrameRate, Tick, TimeRange};
        let mut p = Project::new("Imported");
        let layer = p.add_item(
            "Layer",
            filmcraft_project::Label::Iris,
            ItemKind::AdjustmentLayer { width: 3840, height: 2160, rate: FrameRate::FPS_24, duration: FrameRate::FPS_24.tick_of(48) },
            None,
        );
        let seq = p.new_sequence("Sequence 01", SequenceSettings::default(), 1, 1, None);
        let ti = p.make_track_item(layer, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(48)), FrameRate::FPS_24).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        for pretty in [true, false] {
            let bytes = encode(&p, pretty);
            let text = String::from_utf8(bytes.clone()).unwrap();
            assert!(text.replace([' ', '\n'], "").contains(r#"{"Vec2":{"x":null,"y":null}}"#), "the file holds NaN points as null");
            let l = decode(&bytes).unwrap_or_else(|e| panic!("a file with `null` points is refused: {e}"));
            let motion = l.project.sequence(seq).unwrap().video_tracks[0].items[0].effect("motion").unwrap().clone();
            let anchor = motion.params["anchor"].value.as_vec2().unwrap();
            assert!(anchor.x.is_nan() && anchor.y.is_nan(), "still auto");
            assert_eq!(motion.params["scale"].value.as_f64(), Some(100.0));
            assert_eq!(encode(&l.project, pretty), bytes, "and the file is written back the same");
            // a `null` where no "auto" exists is still a damaged file
            let broken = text.replacen("3840", "null", 1);
            assert!(matches!(decode(broken.as_bytes()), Err(FormatError::Corrupt(_))));
        }
    }

    #[test]
    fn envelope_has_explicit_schema_version() {
        let v: Value = serde_json::from_slice(&encode(&Project::new("x"), true)).unwrap();
        assert_eq!(v["schema_version"], SCHEMA_VERSION);
        assert_eq!(v["format"], FORMAT_ID);
        assert!(v["project"]["items"].is_object());
        assert!(v["project"].get("version").is_none());
    }

    #[test]
    fn refuses_newer() {
        let mut v: Value = serde_json::from_slice(&encode(&Project::new("x"), false)).unwrap();
        v["schema_version"] = Value::from(SCHEMA_VERSION + 1);
        let e = decode(&serde_json::to_vec(&v).unwrap()).unwrap_err();
        assert!(matches!(e, FormatError::TooNew { .. }));
        assert!(e.to_string().contains("newer version of Revolution"), "{e}");
    }

    #[test]
    fn rejects_foreign_and_damaged() {
        assert!(matches!(decode(br#"{"hello": 1}"#), Err(FormatError::NotAProject(_))));
        assert!(matches!(decode(br#"{"format":"other","schema_version":1}"#), Err(FormatError::NotAProject(_))));
        let b = encode(&Project::new("x"), false);
        assert!(matches!(decode(&b[..b.len() / 2]), Err(FormatError::Corrupt(_))));
    }

    #[test]
    fn chain_runs_each_step_in_order() {
        fn a(mut d: Value) -> Result<Value, String> {
            d["trail"] = Value::from(format!("{}a", d["trail"].as_str().unwrap_or("")));
            Ok(d)
        }
        fn b(mut d: Value) -> Result<Value, String> {
            d["trail"] = Value::from(format!("{}b", d["trail"].as_str().unwrap_or("")));
            Ok(d)
        }
        fn fail(_: Value) -> Result<Value, String> {
            Err("boom".into())
        }
        let table: &[Migration] = &[a, b, a];
        let out = migrate_with(table, serde_json::json!({}), 1).unwrap();
        assert_eq!(out["trail"], "aba");
        assert_eq!(out["schema_version"], 4);
        let out = migrate_with(table, serde_json::json!({}), 3).unwrap();
        assert_eq!(out["trail"], "a");
        let out = migrate_with(table, serde_json::json!({"x": 1}), 4).unwrap();
        assert_eq!(out, serde_json::json!({"x": 1}));
        assert!(matches!(migrate_with(table, serde_json::json!({}), 5), Err(FormatError::TooNew { found: 5, supported: 4 })));
        let e = migrate_with(&[a, fail], serde_json::json!({}), 1).unwrap_err();
        assert!(matches!(e, FormatError::Migration { from: 2, .. }), "{e}");
    }

    /// Fork 6.7b: a v16 document without temporal NR comes out of `v16_to_v17` unchanged.
    #[test]
    fn v16_to_v17_leaves_everything_else_alone() {
        let doc = serde_json::json!({
            "format": FORMAT_ID, "schema_version": 16,
            "project": {"items": {
                "1": {"kind": {"Sequence": {"video_tracks": [{"items": [
                    {"name": "a", "grade": {"nodes": [{"id": 1, "params": {}}, {"id": 2, "params": {"gain": {"value": {"Float": 1.0}}}}]}},
                    {"name": "b"}
                ]}], "audio_tracks": [{"items": [{"name": "x", "grade": {"nodes": [{"params": {"tnr.luma": 1}}]}}]}]}}},
                "2": {"kind": {"Media": {}}}
            }}
        });
        assert_eq!(v16_to_v17(doc.clone()).unwrap(), doc);
    }

    /// Fork 6.7b: hostile shapes never panic and never touch other clips.
    #[test]
    fn v16_to_v17_survives_hostile_documents() {
        let tnr = serde_json::json!({"tnr.luma": {"value": {"Float": 0.5}}});
        for doc in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"project": 7}),
            serde_json::json!({"project": {"items": []}}),
            serde_json::json!({"project": {"items": {"1": {"kind": "Sequence"}}}}),
            serde_json::json!({"project": {"items": {"1": {"kind": {"Sequence": {"video_tracks": {"items": 1}}}}}}}),
            serde_json::json!({"project": {"items": {"1": {"kind": {"Sequence": {"video_tracks": [7, {"items": [1, "x", {"grade": 3}, {"grade": {"nodes": 4}}, {"grade": {"nodes": [5, {"params": 6}, {"enabled": "yes", "params": tnr}]}}]}]}}}}}}),
        ] {
            let _ = v16_to_v17(doc);
        }
        // a non-object node before the first enabled one is skipped; an existing clip setting wins;
        // a clip of a non-media item (a nested sequence) gets nothing; stills lose their keys;
        // notes already in the document are appended to
        let doc = serde_json::json!({"migration_notes": ["earlier"], "project": {
        "stills": [{"grade": {"nodes": [{"params": tnr}]}}, 4],
        "items": {"2": {"kind": {"Media": {}}}, "1": {"kind": {"Sequence": {"video_tracks": [{"items": [
            {"item": 2, "grade": {"nodes": [9, {"params": tnr}]}},
            {"item": 2, "temporal_nr": {"params": {"chroma": {"value": {"Float": 0.1}}}}, "grade": {"nodes": [{"params": tnr}]}},
            {"item": 2, "grade": {"bypass": "no", "nodes": [{"params": {"tnr.frames": "two"}}]}},
            {"item": 1, "grade": {"nodes": [{"params": tnr}]}}
        ]}]}}}}}});
        let out = v16_to_v17(doc).unwrap();
        let clips = &out["project"]["items"]["1"]["kind"]["Sequence"]["video_tracks"][0]["items"];
        assert_eq!(clips[0]["temporal_nr"], serde_json::json!({"params": {"luma": {"value": {"Float": 0.5}}}}));
        assert_eq!(clips[0]["grade"]["nodes"][1], serde_json::json!({}));
        assert_eq!(clips[1]["temporal_nr"], serde_json::json!({"params": {"chroma": {"value": {"Float": 0.1}}}}));
        assert!(clips[1]["grade"]["nodes"][0].get("params").is_none());
        // mistyped values move as they are (the clamp handles them at render time)
        assert_eq!(clips[2]["temporal_nr"]["params"]["frames"], "two");
        assert!(clips[3].get("temporal_nr").is_none() && clips[3]["grade"]["nodes"][0].get("params").is_none());
        assert!(out["project"]["stills"][0]["grade"]["nodes"][0].get("params").is_none());
        assert_eq!(out["migration_notes"][0], "earlier");
        assert_eq!(out["migration_notes"][1], "Temporal NR moved from the grade to the clip on 2 clip(s); 3 ignored node setting(s) dropped.");
        // decode never takes notes from the file itself
        let mut forged: serde_json::Value = serde_json::from_slice(&encode(&Project::new("x"), false)).unwrap();
        forged["schema_version"] = 16.into();
        forged["migration_notes"] = serde_json::json!(["forged"]);
        assert!(decode(&serde_json::to_vec(&forged).unwrap()).unwrap().migration_notes.is_empty());
    }

    /// Fork C2 (schema v19): a v18 file's serial grade loads unchanged and saves without any graph
    /// keys; a branched grade (edges, node kinds, output) round-trips.
    #[test]
    fn v18_serial_grades_load_unchanged_and_branched_grades_round_trip() {
        use filmcraft_project::dag::{Edge, Port};
        use filmcraft_project::grade::{Grade, GradeNode, NodeKind, SOURCE};
        let serial = serde_json::json!({"nodes": [{"id": 1, "params": {"saturation": {"value": {"Float": 0.5}}}}, {"id": 2}]});
        let mut doc: Value = serde_json::from_slice(&encode(&Project::new("Grades"), false)).unwrap();
        doc["schema_version"] = 18.into();
        doc["project"]["stills"] = serde_json::json!([{"id": 1, "grade": serial}]);
        let loaded = decode(&serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!(loaded.schema_version, 18);
        let g = loaded.project.stills.first().and_then(|s| s.grade.clone()).unwrap();
        assert!(!g.is_graph() && g.nodes.iter().all(|n| n.kind == NodeKind::Correction));
        let again: Value = serde_json::from_slice(&encode(&loaded.project, false)).unwrap();
        assert_eq!(again["schema_version"], SCHEMA_VERSION);
        let grade = &again["project"]["stills"][0]["grade"];
        assert!(grade.get("edges").is_none() && grade.get("output").is_none());
        assert!(grade["nodes"].as_array().unwrap().iter().all(|n| n.get("kind").is_none()));

        let mut b = Grade { nodes: (1..=3).map(GradeNode::new).collect(), ..Default::default() };
        b.nodes[2].kind = NodeKind::LayerMixer;
        let e = |f: u64, t: u64, i: u8| Edge::new(Port::new(f, 0), Port::new(t, i));
        b.edges = vec![e(SOURCE, 1, 0), e(SOURCE, 2, 0), e(1, 3, 0), e(2, 3, 1)];
        b.output = Some(3);
        let mut p = loaded.project;
        if let Some(s) = p.stills.first_mut() {
            s.grade = Some(b.clone());
        }
        let back = decode(&encode(&p, false)).unwrap();
        assert_eq!(back.project.stills.first().and_then(|s| s.grade.clone()), Some(b));
    }

    /// A comp item (fork C1) saves and loads; one from a hostile file is brought within its caps.
    #[test]
    fn comp_items_round_trip_and_are_sanitized_on_load() {
        use filmcraft_project::comp::{Comp, CompKind, CompNode, MAX_LABEL, MAX_SIDE};
        use filmcraft_project::{ItemKind, Label};
        use filmcraft_time::{FrameRate, Tick};
        let mut p = Project::new("Comps");
        let mut c = Comp::new(1920, 1080, FrameRate::FPS_24, Tick(1000));
        c.nodes.push(CompNode::new(2, CompKind::Solid));
        let id = p.add_item("Comp 1", Label::Iris, ItemKind::Comp(std::sync::Arc::new(c.clone())), None);
        let back = decode(&encode(&p, false)).unwrap();
        assert!(matches!(&back.project.item(id).unwrap().kind, ItemKind::Comp(b) if **b == c));

        let mut doc: Value = serde_json::from_slice(&encode(&p, false)).unwrap();
        let comp = &mut doc["project"]["items"][id.0.to_string()]["kind"]["Comp"];
        comp["width"] = 4_000_000_000u32.into();
        comp["nodes"][1]["label"] = "x".repeat(500).into();
        let back = decode(&serde_json::to_vec(&doc).unwrap()).unwrap();
        let ItemKind::Comp(b) = &back.project.item(id).unwrap().kind else { panic!("not a comp") };
        assert_eq!(b.width, MAX_SIDE);
        assert_eq!(b.node(2).unwrap().label.chars().count(), MAX_LABEL);
    }

    /// Fork C1: random, hostile comps inside a project never panic on load, and whatever loads is
    /// within the caps and validates (or reports why not) without panicking.
    #[test]
    fn hostile_random_comps_never_panic_and_hold_the_caps() {
        use filmcraft_project::ItemKind;
        use filmcraft_project::comp::{KINDS, MAX_LABEL, MAX_PIXELS, MAX_SIDE};
        use serde_json::json;
        let base: Value = serde_json::from_slice(&encode(&Project::new("Fuzz"), false)).unwrap();
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let numbers = [json!(0), json!(-1), json!(1), json!(64), json!(u32::MAX), json!(1e300), json!(null), json!("x")];
        let mut loaded = 0;
        for round in 0..400 {
            let mut pick = |n: u64| next() % n.max(1);
            let nodes: Vec<Value> = (0..pick(12))
                .map(|_| {
                    let kind = KINDS[pick(KINDS.len() as u64) as usize];
                    let mut k = json!({"type": kind});
                    match kind {
                        "mediaIn" => k["source"] = json!({"kind": "item", "item": pick(5)}),
                        "key" => k["effect"] = json!(["ultra_key", "luma_key", "nope"][pick(3) as usize]),
                        "color" => k["node"] = json!({"id": pick(3), "params": {"saturation": {"value": {"Float": 2.0}}}}),
                        _ => {}
                    }
                    json!({
                        "id": pick(10),
                        "kind": k,
                        "label": "L".repeat(pick(200) as usize),
                        "pos": [numbers[pick(8) as usize].clone(), 0.0],
                        "params": {"opacity": {"value": {"Float": 50.0}}, "color": {"value": {"Text": "t".repeat(pick(9000) as usize)}}},
                    })
                })
                .collect();
            let edges: Vec<Value> =
                (0..pick(20)).map(|_| json!({"from": {"node": pick(10), "index": pick(3)}, "to": {"node": pick(10), "index": pick(4)}})).collect();
            let comp = json!({
                "width": numbers[pick(8) as usize].clone(),
                "height": numbers[pick(8) as usize].clone(),
                "rate": {"num": pick(60) as i64 - 5, "den": pick(3) as i64 - 1},
                "duration": pick(1000) as i64 - 500,
                "nodes": nodes,
                "edges": edges,
                "output": pick(10),
            });
            let mut doc = base.clone();
            doc["project"]["items"]["99"] = json!({"id": 99, "name": format!("c{round}"), "label": "Iris", "created": 99, "kind": {"Comp": comp}});
            let Ok(l) = decode(&serde_json::to_vec(&doc).unwrap()) else { continue };
            let Some(ItemKind::Comp(c)) = l.project.item(filmcraft_project::ItemId(99)).map(|i| &i.kind) else { continue };
            assert!((1..=MAX_SIDE).contains(&c.width) && (1..=MAX_SIDE).contains(&c.height));
            assert!(u64::from(c.width) * u64::from(c.height) <= MAX_PIXELS);
            assert!(c.nodes.iter().all(|n| n.label.chars().count() <= MAX_LABEL && n.pos.iter().all(|v| v.is_finite())));
            assert!(c.rate.num > 0 && c.rate.den > 0 && c.duration.0 >= 0);
            let _ = c.validate();
            loaded += 1;
        }
        // the fuzzer must reach the comp checks, not only serde errors
        assert!(loaded > 20, "only {loaded} of 400 random comps loaded");
    }
}
