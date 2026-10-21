//! "How do I get this look?" from a picture you like (fork AI-PLAN Phase REF, items 2–5).
//!
//! A reference picture (`stills.import`, or any Gallery still) is measured against the current
//! shot ([`filmcraft_render::look_diff`]); FilmCraft then writes the answer's skeleton itself,
//! never the model: the measured "theirs vs yours" lines, a grade recipe whose every step names
//! the guide section it comes from, what belongs on set (light, lens, haze) rather than in the
//! grade, a warning when the two pictures are too different for a grade to bridge, and the
//! "Do this" first pass (`grade.shotMatch` with the still: one ordinary node).
//!
//! It never claims to know how the reference was made: the recipe is "what would move your
//! shot towards it", worded as such. The picture never leaves the machine (no AI5 share, no
//! training data: see `coach_share`).

use filmcraft_render::look_diff::{self, LookDiff, LookFacts, LookTag};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq};
use crate::{Result, Session};

/// Mids this many stops apart: a grade only gets part of the way (day vs night).
pub const TOO_DIFFERENT_STOPS: f32 = 2.5;
/// Saturation ratio beyond this (either way) together with a strong cast change: too different.
pub const TOO_DIFFERENT_SATURATION: f32 = 2.0;
/// A near-neutral cast change of this many JND casts counts as a strong one.
pub const TOO_DIFFERENT_CAST_JND: f32 = 15.0;

/// One grade step of the recipe and the guide section it comes from.
#[derive(Clone, Debug, PartialEq)]
pub struct RefStep {
    pub text: &'static str,
    /// The guide section id (`> id:` of a section in `docs/guide/`).
    pub see: &'static str,
}

/// The FilmCraft-written part of a reference answer.
#[derive(Clone, Debug, PartialEq)]
pub struct RefAnswer {
    pub still: u64,
    pub clip: u64,
    pub tags: Vec<LookTag>,
    /// "Theirs vs yours (measured)" lines.
    pub measured: Vec<String>,
    /// In the grade, in order (the first is always Shot Match on the still).
    pub grade: Vec<RefStep>,
    /// On set: what light, lens or atmosphere would do that a grade can't.
    pub on_set: Vec<&'static str>,
    /// Set when the pictures are too far apart for a grade to get there.
    pub too_different: Option<String>,
}

impl RefAnswer {
    /// The answer as JSON (`coach.reference`, and `reference` in an assistant answer).
    pub fn to_json(&self) -> Value {
        json!({
            "still": self.still,
            "clip": self.clip,
            "tags": self.tags.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
            "measured": self.measured,
            "grade": self.grade.iter().map(|s| json!({"text": s.text, "see": s.see})).collect::<Vec<_>>(),
            "onSet": self.on_set,
            "tooDifferent": self.too_different,
            "doThis": {"label": "Shot Match to the reference", "command": "grade.shotMatch", "params": {"clip": self.clip, "still": self.still}},
        })
    }

    /// The answer as plain text for the assistant's prompt and the Ask box: both parts, always.
    pub fn text(&self) -> String {
        let mut out = String::from("In the grade:\n");
        for (i, s) in self.grade.iter().enumerate() {
            out.push_str(&format!("{}. {} [{}]\n", i + 1, s.text, s.see));
        }
        out.push_str("On set:\n");
        for l in &self.on_set {
            out.push_str(&format!("- {l}\n"));
        }
        if let Some(t) = &self.too_different {
            out.push_str(&format!("Note: {t}\n"));
        }
        out
    }
}

/// The grade step for one measured difference (None: nothing to do in the grade for it).
fn grade_step(tag: LookTag) -> Option<RefStep> {
    use LookTag::*;
    let (text, see) = match tag {
        Brighter | Darker => ("Set the exposure first: move the Gain (or Offset) master so the mids sit where theirs do.", "practice.grading.balance-first"),
        MoreContrast | LessContrast => {
            ("Match the contrast with Contrast and Pivot on the primaries, keeping the mids in place.", "practice.grading.contrast-pivot")
        }
        FadedBlacks => {
            ("Lift the blacks: raise the bottom point of the master curve until the darkest areas turn soft grey like theirs.", "practice.looks.faded-blacks")
        }
        CrushedBlacks => {
            ("Deepen the blacks: pull the bottom of the master curve (or Lift) down until the shadows are as dense as theirs.", "color.curves.steps")
        }
        WarmerOverall => ("Warm the whole picture: Temperature up (or the Gain wheel towards orange) on the look node.", "color.recipes.warm"),
        CoolerOverall => ("Cool the whole picture: Temperature down (or the Gain wheel towards blue) on the look node.", "color.recipes.cool"),
        TealShadows | OrangeHighlights | WarmShadows | CoolHighlights | SplitToneTealOrange => (
            "Split-tone: push the Lift wheel and the Gain wheel towards the shadow and highlight colours the measurement names.",
            "practice.grading.split-tone",
        ),
        MoreSaturated | LessSaturated => ("Match the saturation with Saturation on the look node (after the balance, before grain).", "color.primaries.panel"),
        SkinWarmer | SkinCooler => {
            ("Bring the skin to their hue with a skin qualifier and a small Hue or Gain move, watching the skin line.", "practice.grading.skin-line")
        }
        Letterbox | Pillarbox => ("Frame like theirs: a Crop effect (top and bottom, or left and right) gives the same black bars.", "effects.apply"),
        MoreGrain => ("Add grain on the last node to get their texture.", "color.finishing.grain"),
        LessGrain => ("Theirs is cleaner: reduce noise before the grade.", "color.noise.why"),
    };
    Some(RefStep { text, see })
}

/// What happens on set for a measured difference (None: the grade covers it).
fn set_note(tag: LookTag) -> Option<&'static str> {
    use LookTag::*;
    Some(match tag {
        MoreContrast => {
            "More contrast often comes from the light itself: one harder key light, less fill, darker surroundings. A grade deepens contrast but can't add shape that wasn't lit."
        }
        LessContrast => "Softer contrast usually means more fill or a bigger, diffused source; lifting shadows in the grade also lifts their noise.",
        Darker => "A darker, moodier frame is easier when it is lit that way: expose for the highlights and keep light off the background.",
        WarmerOverall | OrangeHighlights => {
            "Warm light on set (low sun, tungsten, a warm gel) gives warmth with natural falloff; the grade can only tint what is there."
        }
        CoolerOverall | TealShadows => "Cool shadows on set come from a cool fill or ambient light (blue hour, daylight through a window against a warm key).",
        Letterbox | Pillarbox => "Frame for the wider shape while shooting (aspect-ratio guides on the monitor), or the crop will cut into your framing.",
        MoreGrain => "Their grain may come from film or a high ISO; a grain node gets close, especially at the final delivery size.",
        _ => return None,
    })
}

/// Lines that are always true about a reference: what a grade can't do.
const ALWAYS_ON_SET: &str = "Where the light comes from, how soft it is, depth of field and haze are set by the lighting and the lens; a grade can only fake a little of them (a vignette, a soft glow, a lens blur with a mask).";

/// The recipe for `theirs` (the reference) against `yours` (the shot).
pub fn recipe(theirs: &LookFacts, yours: &LookFacts, still: u64, clip: u64) -> RefAnswer {
    let diff: LookDiff = look_diff::difference(theirs, yours);
    let mut grade = vec![RefStep {
        text: "Start with Shot Match to the reference picture: it adds one ordinary node you can see, adjust or switch off.",
        see: "color.matching.shot-match",
    }];
    let mut on_set: Vec<&'static str> = Vec::new();
    for t in &diff.tags {
        if let Some(s) = grade_step(*t)
            && !grade.contains(&s)
        {
            grade.push(s);
        }
        if let Some(n) = set_note(*t)
            && !on_set.contains(&n)
        {
            on_set.push(n);
        }
    }
    on_set.push(ALWAYS_ON_SET);
    RefAnswer { still, clip, tags: diff.tags, measured: diff.lines, grade, on_set, too_different: too_different(theirs, yours) }
}

/// Why a grade alone won't get there, when the pictures are that far apart.
pub fn too_different(theirs: &LookFacts, yours: &LookFacts) -> Option<String> {
    let (t, y) = (&theirs.facts, &yours.facts);
    let stops = t.mids_stops - y.mids_stops;
    if stops.is_finite() && stops.abs() >= TOO_DIFFERENT_STOPS {
        return Some(format!(
            "Their picture is about {:.0} stops {} than yours in the mids (like night against day). A grade gets part of the way; the rest is in how the scene is lit.",
            stops.abs(),
            if stops < 0.0 { "darker" } else { "brighter" }
        ));
    }
    // (a nearly grey shot against a colourful reference counts as far apart in saturation)
    let ratio = if y.chroma > 1e-4 {
        t.chroma / y.chroma
    } else if t.chroma > 1e-4 {
        TOO_DIFFERENT_SATURATION
    } else {
        1.0
    };
    let cast = (t.cast[1] - y.cast[1]).abs().max((t.cast[0] - y.cast[0]).abs());
    let both_neutral =
        t.neutral_share >= filmcraft_render::picture_facts::NEUTRAL_MIN_SHARE && y.neutral_share >= filmcraft_render::picture_facts::NEUTRAL_MIN_SHARE;
    let strong_cast = both_neutral && cast >= TOO_DIFFERENT_CAST_JND * filmcraft_render::grade_match::JND_CAST;
    let sat_far = ratio.is_finite() && (ratio >= TOO_DIFFERENT_SATURATION || ratio <= 1.0 / TOO_DIFFERENT_SATURATION);
    (strong_cast && sat_far).then(|| {
        "Their colours are far from yours (a very different light, like neon against daylight). Pushing a grade that far breaks skin and noise; expect to change the light for the rest.".to_string()
    })
}

/// The reference answer for `still` against the clip at the playhead (or `p["clip"]`).
pub fn reference_answer(s: &Session, still: u64, p: &Value, cmd: &str) -> Result<RefAnswer> {
    let (theirs, yours, clip) = crate::grade_tools::reference_looks(s, still, p, cmd)?;
    Ok(recipe(&theirs, &yours, still, clip.0))
}

/// Words that make a question about a reference look ("how do I get this look?").
/// (Phrases, not single words: "why does the face look green?" or "how do I match these two
/// clips?" are not about the reference.)
const LOOK_WORDS: [&str; 10] =
    ["this look", "that look", "the look", "reference", "like this", "like that", "this style", "that style", "this photo", "this picture"];

/// AI-PLAN REF: the reference still a question is about: `reference` when given, else (with
/// `auto`) the Gallery still being compared when it is an imported reference picture and the
/// question asks about a look.
pub fn reference_for(s: &Session, p: &Value, question: &str, auto: bool) -> Result<Option<u64>> {
    if let Some(v) = p.get("reference") {
        let id = v.as_u64().ok_or_else(|| bad("coach.assist", "`reference` must be a still id"))?;
        if !s.project.stills.iter().any(|x| x.id == id) {
            return Err(bad("coach.assist", format!("no still {id}")));
        }
        return Ok(Some(id));
    }
    if !auto {
        return Ok(None);
    }
    let q = question.to_lowercase();
    let about_look = LOOK_WORDS.iter().any(|w| q.contains(w));
    Ok(s.state.compare.still.filter(|id| about_look && s.project.stills.iter().any(|x| x.id == *id && x.is_reference())))
}

/// The vision model's instruction for a reference picture (item 4): what numbers can't say.
pub fn reference_look_prompt(question: &str) -> String {
    format!(
        "This is a reference picture someone wants their own video to look like. They ask: \"{}\". In two or three plain sentences, \
         describe only what you can see: where the light comes from and how hard or soft it is, depth of field (sharp or blurred \
         background), haze, bloom or halation around highlights, grain, and the framing. Don't guess the camera, lens or settings, \
         and don't give advice.",
        filmcraft_llm::sanitize(&question.trim().chars().filter(|c| !c.is_control()).take(300).collect::<String>())
    )
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "coach.reference",
        label: "How Do I Get This Look?",
        menu: &[],
        shortcut: None,
        params: r#"{"still":id,"clip":id?,"time":ticks?}"#,
        enabled: has_seq,
        run: reference_cmd,
        journal: false,
    }]
}

fn reference_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "coach.reference";
    let still = p.get("still").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "need `still` (a Gallery still, e.g. an imported reference picture)"))?;
    let a = reference_answer(s, still, p, CMD)?;
    let mut v = a.to_json();
    v["text"] = json!(a.text());
    Ok(v)
}
