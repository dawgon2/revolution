//! Clip temporal noise reduction rows (fork 6.7b, design `docs/fork/DESIGN-6.7b.md` §1.7): an
//! Enabled switch and the luma / chroma / frames / motion rows of the selected clip's
//! `temporal_nr`, shared by Effect Controls (the Inspector in the Resolve workflow) and the Color
//! page's Noise tab, so both always agree. Every edit is a `tnr.*` command.
//!
//! Automation ids: `tnr.enabled`, and `tnr.<param>` with `.reset` and `.anim` for `luma`,
//! `chroma`, `frames`, `motion`.

use egui::Rect;
use filmcraft_engine::Session;
use filmcraft_project::grade::NodeParamKind;
use filmcraft_project::{ClipId, TemporalNr, TrackItem, tnr_param};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::grade::{FloatRow, RowEdit, float_row};

/// The rows: parameter key, then the automation ids of its slider, reset button and keyframe
/// toggle (literal ids, so the guide's `{ui:tnr.*}` names are plain source strings).
/// (param key, on-screen label, automation ids). The rows sit under a "Temporal NR" heading, so they
/// use the short words instead of the params' "Temporal NR …" names (which overflowed the label
/// column).
const ROWS: [(&str, &str, &str, &str, &str); 4] = [
    ("luma", "Luma", "tnr.luma", "tnr.luma.reset", "tnr.luma.anim"),
    ("chroma", "Chroma", "tnr.chroma", "tnr.chroma.reset", "tnr.chroma.anim"),
    ("frames", "Frames", "tnr.frames", "tnr.frames.reset", "tnr.frames.anim"),
    ("motion", "Motion", "tnr.motion", "tnr.motion.reset", "tnr.motion.anim"),
];

/// Whether clip `c` of the active sequence takes a temporal NR: on a video track, and footage by
/// the one rule the engine's `tnr.*` commands and the render use
/// ([`filmcraft_render::grade::takes_temporal_nr`]), so the rows show only where they work.
pub(crate) fn takes_tnr(s: &Session, c: ClipId) -> bool {
    let Some(q) = s.active_sequence() else { return false };
    if !q.video_tracks.iter().any(|t| t.item(c).is_some()) {
        return false;
    }
    let Some((_, it)) = q.find_item(c) else { return false };
    filmcraft_render::grade::takes_temporal_nr(&s.project, it.item)
}

/// The Enabled switch and the four rows for clip `clip` (`it`) at media time `mt`. Commands go
/// to `cmds` (with `clip` set); a slider drag uses `merge` as the `tnr.set` merge key, so pass
/// one that changes on every press (the grading panels' gesture number) to make a drag one undo
/// step.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tnr_rows(
    ui: &mut egui::Ui,
    clip: ClipId,
    it: &TrackItem,
    mt: Tick,
    t: &crate::theme::Tokens,
    merge: &str,
    cmds: &mut Vec<(&'static str, Value)>,
    autos: &mut Vec<(String, Rect, String)>,
) {
    let present = it.temporal_nr.is_some();
    let tnr = it.temporal_nr.clone().unwrap_or_default();
    let mut on = present && tnr.enabled;
    let label = tl!("Enabled");
    let r = ui.checkbox(&mut on, label);
    autos.push(("tnr.enabled".to_string(), r.rect, label.to_string()));
    if r.changed() {
        cmds.push(("tnr.toggle", json!({"clip": clip.0, "on": on})));
    }
    for (key, label, id, reset_id, anim_id) in ROWS {
        let Some(def) = tnr_param(key) else { continue };
        let NodeParamKind::Float { min, max, .. } = def.kind else { continue };
        let row = FloatRow {
            label: crate::i18n::t(label),
            value: tnr.float_at(key, mt),
            min,
            max,
            // (whole frames for the temporal radius)
            step: if key == "frames" { 1.0 } else { 0.0 },
            suffix: "",
            enabled: true,
            animated: Some(animated(&tnr, key)),
            id,
            reset_id,
            anim_id,
        };
        match float_row(ui, t, row, autos) {
            Some(RowEdit::Set(v)) => cmds.push(("tnr.set", json!({"clip": clip.0, "param": key, "value": v, "merge": merge}))),
            Some(RowEdit::ToggleAnimation) => cmds.push(("tnr.toggleAnimation", json!({"clip": clip.0, "param": key}))),
            // (nothing to reset on a clip without temporal NR)
            Some(RowEdit::Reset) if present => cmds.push(("tnr.reset", json!({"clip": clip.0, "param": key}))),
            Some(RowEdit::Reset) | None => {}
        }
    }
}

fn animated(t: &TemporalNr, key: &str) -> bool {
    t.params.get(key).is_some_and(|p| p.is_animated())
}

/// The note under the rows: where the temporal NR shows.
pub(crate) fn note(ui: &mut egui::Ui, t: &crate::theme::Tokens) {
    ui.label(egui::RichText::new(tl!("Shown on the paused frame, at export and in the render cache; not while playing.")).color(t.text_faint));
}
