//! The Grading Coach's guided lessons (fork GRADE-PLAN G2.12, part 1).
//!
//! A lesson is a data file (`assets/lessons/*.toml`, embedded at build time) of steps. Each step
//! says what to do (`text`) and why (`why`), names the control to highlight (`target`, an
//! automation id of the UI) and what finishes it (`expect`):
//!
//! - `{ command = "grade.setParam", params = { param = "saturation", value = { gt = 1.0 } } }`:
//!   a command that succeeded with parameters matching the pattern (see [`matches`]);
//! - `{ element = "grade.addNode" }`: a control that is on screen (the UI reports what it shows
//!   each frame; agents can call `coach.seen`);
//! - `{ manual = true }`: the reader presses Next.
//!
//! A step may also say when it is already done before the reader does anything, with
//! `already = "clipSelected"` (a clip on a video track is selected): such a step finishes by
//! itself as soon as it starts (or on `coach.state`) when that holds, so a "select a clip" step
//! doesn't wait for a click on a clip that is already selected (clicking it again sends no
//! command). A step reached with Back is never finished this way (the reader went back to it).
//!
//! The runner ([`Coach`]) watches every command [`Session::execute`] runs, so lessons advance
//! the same way whether the user clicks, an agent drives the control channel or a script runs.
//! Back / Skip / Exit work at any time; finished lessons are remembered in the preferences
//! (`color.coach.completed`). Commands: `coach.lessons`, `coach.start`, `coach.state`,
//! `coach.next`, `coach.back`, `coach.skip`, `coach.exit`, `coach.seen`.
//!
//! `target` and `expect.command` take one id or a list of up to [`MAX_ALTERNATIVES`]: the UI
//! highlights the first target that is on screen, and any of the commands finishes the step (an
//! already graded clip shows "+ Serial" where an ungraded one shows "Add Node").
//!
//! The runner only moves forward on what the user does, never backward: undoing a command
//! (`edit.undo`) does not take a lesson back to the step that command finished. Use Back.
//!
//! Lesson files are treated as hostile input: size, counts, lengths, characters and the depth of
//! parameter patterns are bounded, and unknown fields are refused.

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, str_p};
use crate::{Result, Session};

/// Largest lesson file accepted, in bytes.
pub const MAX_FILE_BYTES: usize = 64 * 1024;
/// Most steps in one lesson.
pub const MAX_STEPS: usize = 40;
/// Longest step text / why, in characters.
pub const MAX_TEXT: usize = 1200;
/// Longest title, in characters.
pub const MAX_TITLE: usize = 120;
/// Longest summary, in characters.
pub const MAX_SUMMARY: usize = 600;
/// Longest lesson id, automation id or command id.
pub const MAX_ID: usize = 128;
/// Deepest nesting of a parameter pattern.
pub const MAX_PATTERN_DEPTH: usize = 6;
/// Most values in one parameter pattern.
pub const MAX_PATTERN_NODES: usize = 64;
/// Most ids in a `target` or `expect.command` list.
pub const MAX_ALTERNATIVES: usize = 8;
/// Most finished lessons remembered in the preferences.
pub const MAX_COMPLETED: usize = 256;

/// The comparison operators a pattern object may use (an object whose keys are all operators).
const OPS: &[&str] = &["gt", "ge", "lt", "le", "ne", "any", "nonzero", "in"];

/// Where a lesson's feature comes from (the fork guide's provenance tags, `docs/guide/README.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    ResolveStyle,
    Filmcraft,
    Fork,
    Standard,
}

impl Provenance {
    /// The tag as the guide writes it.
    pub fn tag(self) -> &'static str {
        match self {
            Provenance::ResolveStyle => "[Resolve-style]",
            Provenance::Filmcraft => "[Revolution]",
            Provenance::Fork => "[Fork]",
            Provenance::Standard => "[Standard]",
        }
    }
}

/// A condition on the current state that means a step is already done (`already = "…"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Already {
    /// A clip on a video track of the open sequence is selected.
    ClipSelected,
}

/// What finishes a step.
#[derive(Clone, Debug, PartialEq)]
pub enum Expect {
    /// One of these commands succeeded, with parameters matching `params` (None: any parameters).
    Command { ids: Vec<String>, params: Option<Value> },
    /// A UI element (automation id) that is on screen.
    Element(String),
    /// The reader presses Next.
    Manual,
}

impl Expect {
    pub fn to_json(&self) -> Value {
        match self {
            Expect::Command { ids, params } => json!({"kind": "command", "command": ids.first(), "commands": ids, "params": params}),
            Expect::Element(e) => json!({"kind": "element", "element": e}),
            Expect::Manual => json!({"kind": "manual"}),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// What to do, in plain words.
    pub text: String,
    /// Why it matters (shown with the step; may be empty).
    pub why: String,
    /// The controls to highlight (automation ids, the first on screen wins; empty: none).
    pub targets: Vec<String>,
    pub expect: Expect,
    /// Done by itself when this holds as the step starts (see the module docs).
    pub already: Option<Already>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lesson {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub provenance: Vec<Provenance>,
    /// Position in the lesson list (lower first).
    pub order: u32,
    pub steps: Vec<Step>,
}

// ---------- the file format ----------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLesson {
    id: String,
    title: String,
    #[serde(default)]
    summary: String,
    provenance: Vec<Provenance>,
    #[serde(default)]
    order: u32,
    #[serde(rename = "step")]
    steps: Vec<RawStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    text: String,
    #[serde(default)]
    why: String,
    target: Option<OneOrMany>,
    expect: RawExpect,
    already: Option<Already>,
}

/// One id, or a list of them.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    /// The ids, each valid and at most [`MAX_ALTERNATIVES`] of them.
    pub(crate) fn ids(self, what: &str) -> std::result::Result<Vec<String>, String> {
        let v = match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        };
        if v.is_empty() || v.len() > MAX_ALTERNATIVES {
            return Err(format!("{what} needs 1 to {MAX_ALTERNATIVES} ids"));
        }
        if !v.iter().all(|s| valid_id(s)) {
            return Err(format!("{what} is not an id"));
        }
        Ok(v)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExpect {
    command: Option<OneOrMany>,
    params: Option<Value>,
    element: Option<String>,
    manual: Option<bool>,
}

/// An automation or command id: ASCII letters, digits and `._-`, 1…[`MAX_ID`] long.
pub(crate) fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Text a lesson shows: within `max` characters, no control characters other than new lines.
pub(crate) fn check_text(what: &str, s: &str, max: usize, required: bool) -> std::result::Result<(), String> {
    if required && s.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    if s.chars().any(|c| c.is_control() && c != '\n') {
        return Err(format!("{what} has control characters"));
    }
    Ok(())
}

/// Check a parameter pattern: bounded depth and size, known operators with values of the right type.
pub(crate) fn check_pattern(v: &Value, depth: usize, nodes: &mut usize) -> std::result::Result<(), String> {
    *nodes += 1;
    if *nodes > MAX_PATTERN_NODES {
        return Err(format!("the params pattern has more than {MAX_PATTERN_NODES} values"));
    }
    if depth > MAX_PATTERN_DEPTH {
        return Err(format!("the params pattern is nested deeper than {MAX_PATTERN_DEPTH}"));
    }
    match v {
        Value::Object(m) if is_op(m) => {
            for (k, x) in m {
                let ok = match k.as_str() {
                    "gt" | "ge" | "lt" | "le" | "ne" => x.as_f64().is_some_and(f64::is_finite),
                    "any" | "nonzero" => x.is_boolean(),
                    "in" => x.as_array().is_some_and(|a| !a.is_empty() && a.len() <= 16 && a.iter().all(|e| !e.is_object() && !e.is_array())),
                    _ => false,
                };
                if !ok {
                    return Err(format!("operator `{k}` has a value of the wrong kind"));
                }
            }
            Ok(())
        }
        Value::Object(m) => m.values().try_for_each(|x| check_pattern(x, depth + 1, nodes)),
        Value::Array(a) => a.iter().try_for_each(|x| check_pattern(x, depth + 1, nodes)),
        Value::Number(n) if n.as_f64().is_none_or(|f| !f.is_finite()) => Err("a number in the params pattern is not finite".into()),
        Value::String(s) if s.chars().count() > MAX_TEXT => Err("a string in the params pattern is too long".into()),
        _ => Ok(()),
    }
}

fn is_op(m: &serde_json::Map<String, Value>) -> bool {
    !m.is_empty() && m.keys().all(|k| OPS.contains(&k.as_str()))
}

/// Parse and validate a lesson file. Errors name the step (1-based) and what is wrong.
pub fn parse(src: &str) -> std::result::Result<Lesson, String> {
    if src.len() > MAX_FILE_BYTES {
        return Err(format!("the lesson file is larger than {MAX_FILE_BYTES} bytes"));
    }
    let raw: RawLesson = toml::from_str(src).map_err(|e| e.to_string())?;
    if !valid_id(&raw.id) || raw.id.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(format!("lesson id `{}` must be lower-case letters, digits, `.`, `_` or `-`", raw.id.chars().take(MAX_ID).collect::<String>()));
    }
    check_text("the title", &raw.title, MAX_TITLE, true)?;
    check_text("the summary", &raw.summary, MAX_SUMMARY, false)?;
    if raw.provenance.is_empty() || raw.provenance.len() > 4 {
        return Err("provenance needs one to four tags".into());
    }
    if raw.steps.is_empty() || raw.steps.len() > MAX_STEPS {
        return Err(format!("a lesson needs 1 to {MAX_STEPS} steps"));
    }
    let mut steps = Vec::with_capacity(raw.steps.len());
    for (i, s) in raw.steps.into_iter().enumerate() {
        let n = i + 1;
        check_text(&format!("step {n}'s text"), &s.text, MAX_TEXT, true)?;
        check_text(&format!("step {n}'s why"), &s.why, MAX_TEXT, false)?;
        let targets = match s.target {
            Some(t) => t.ids("target").map_err(|m| format!("step {n}: {m} (an automation id)"))?,
            None => Vec::new(),
        };
        let e = s.expect;
        let kinds = usize::from(e.command.is_some()) + usize::from(e.element.is_some()) + usize::from(e.manual.is_some());
        if kinds != 1 {
            return Err(format!("step {n}: expect needs exactly one of `command`, `element` or `manual`"));
        }
        if e.params.is_some() && e.command.is_none() {
            return Err(format!("step {n}: `params` only goes with `command`"));
        }
        let expect = if let Some(c) = e.command {
            let ids = c.ids("`command`").map_err(|m| format!("step {n}: {m} (a command id)"))?;
            if let Some(p) = &e.params {
                if !p.is_object() {
                    return Err(format!("step {n}: `params` must be a table"));
                }
                check_pattern(p, 0, &mut 0).map_err(|m| format!("step {n}: {m}"))?;
            }
            Expect::Command { ids, params: e.params }
        } else if let Some(el) = e.element {
            if !valid_id(&el) {
                return Err(format!("step {n}: `element` is not an automation id"));
            }
            Expect::Element(el)
        } else if e.manual == Some(true) {
            Expect::Manual
        } else {
            return Err(format!("step {n}: `manual` must be true"));
        };
        steps.push(Step { text: s.text, why: s.why, targets, expect, already: s.already });
    }
    Ok(Lesson { id: raw.id, title: raw.title, summary: raw.summary, provenance: raw.provenance, order: raw.order, steps })
}

/// The lesson files shipped with the app (name, contents).
pub const BUNDLED_FILES: &[(&str, &str)] = &[
    ("nodes-and-flow.toml", include_str!("../../../assets/lessons/nodes-and-flow.toml")),
    ("first-grade.toml", include_str!("../../../assets/lessons/first-grade.toml")),
    ("which-node.toml", include_str!("../../../assets/lessons/which-node.toml")),
    ("match-shots.toml", include_str!("../../../assets/lessons/match-shots.toml")),
    ("fix-skin.toml", include_str!("../../../assets/lessons/fix-skin.toml")),
    ("tracked-window.toml", include_str!("../../../assets/lessons/tracked-window.toml")),
    ("log-noise.toml", include_str!("../../../assets/lessons/log-noise.toml")),
    ("first-comp.toml", include_str!("../../../assets/lessons/first-comp.toml")),
    ("key-comp.toml", include_str!("../../../assets/lessons/key-comp.toml")),
    ("parallel-nodes.toml", include_str!("../../../assets/lessons/parallel-nodes.toml")),
    ("layer-look.toml", include_str!("../../../assets/lessons/layer-look.toml")),
    ("splitter-combiner.toml", include_str!("../../../assets/lessons/splitter-combiner.toml")),
    ("outside-node.toml", include_str!("../../../assets/lessons/outside-node.toml")),
    ("key-mixer.toml", include_str!("../../../assets/lessons/key-mixer.toml")),
    ("track-effects.toml", include_str!("../../../assets/lessons/track-effects.toml")),
];

/// The bundled lessons that parse, in lesson order (a broken file is left out and logged; a test
/// keeps them all valid).
pub fn bundled() -> &'static [Arc<Lesson>] {
    static LESSONS: OnceLock<Vec<Arc<Lesson>>> = OnceLock::new();
    LESSONS.get_or_init(|| {
        let mut v: Vec<Arc<Lesson>> = Vec::new();
        for (name, src) in BUNDLED_FILES {
            match parse(src) {
                Ok(l) if !v.iter().any(|x| x.id == l.id) => v.push(Arc::new(l)),
                Ok(l) => log::warn!("lesson {name}: duplicate id {}", l.id),
                Err(e) => log::warn!("lesson {name}: {e}"),
            }
        }
        v.sort_by_key(|l| l.order);
        v
    })
}

pub fn lesson(id: &str) -> Option<Arc<Lesson>> {
    bundled().iter().find(|l| l.id == id).cloned()
}

// ---------- matching ----------

fn num_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn nonzero(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().is_some_and(|f| f.abs() > 1e-6),
        Value::Array(a) => a.iter().any(nonzero),
        _ => false,
    }
}

/// Whether `actual` matches `pattern`:
/// - a table matches when every key of the pattern matches the same key of `actual` (extra keys
///   in `actual` are fine);
/// - a table whose keys are all operators compares: `gt` / `ge` / `lt` / `le` / `ne` (numbers),
///   `any = true` (the value is present), `nonzero = true` (a number, or any number in a list,
///   other than 0), `in = [..]` (equal to one of them);
/// - a list matches element by element (`actual` may be longer);
/// - anything else must be equal (numbers within a rounding error).
pub fn matches(pattern: &Value, actual: &Value) -> bool {
    matches_at(pattern, actual, 0)
}

fn matches_at(pattern: &Value, actual: &Value, depth: usize) -> bool {
    if depth > MAX_PATTERN_DEPTH {
        return false;
    }
    match pattern {
        Value::Object(m) if is_op(m) => m.iter().all(|(op, want)| {
            let n = actual.as_f64();
            let w = want.as_f64();
            match op.as_str() {
                "gt" => matches!((n, w), (Some(a), Some(b)) if a > b),
                "ge" => matches!((n, w), (Some(a), Some(b)) if a >= b),
                "lt" => matches!((n, w), (Some(a), Some(b)) if a < b),
                "le" => matches!((n, w), (Some(a), Some(b)) if a <= b),
                "ne" => matches!((n, w), (Some(a), Some(b)) if !num_eq(a, b)),
                "any" => want.as_bool() == Some(!actual.is_null()),
                "nonzero" => want.as_bool() == Some(nonzero(actual)),
                "in" => want.as_array().is_some_and(|a| a.iter().any(|x| matches_at(x, actual, depth + 1))),
                _ => false,
            }
        }),
        Value::Object(m) => m.iter().all(|(k, want)| matches_at(want, actual.get(k).unwrap_or(&Value::Null), depth + 1)),
        Value::Array(a) => actual.as_array().is_some_and(|b| b.len() >= a.len() && a.iter().zip(b).all(|(x, y)| matches_at(x, y, depth + 1))),
        Value::Number(n) => match (n.as_f64(), actual.as_f64()) {
            (Some(a), Some(b)) => num_eq(a, b),
            _ => false,
        },
        other => other == actual,
    }
}

// ---------- the runner ----------

/// Grading Coach preferences (Settings ▸ Color ▸ Grading Coach), kept in `preferences.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CoachPrefs {
    /// Context hints the first time something confusing comes up (off with this one switch).
    #[serde(deserialize_with = "lenient_hints")]
    pub hints: bool,
    /// Ids of the lessons finished, oldest first (at most [`MAX_COMPLETED`]).
    #[serde(deserialize_with = "lenient_completed")]
    pub completed: Vec<String>,
    /// Ids of the context hints already shown, oldest first (at most
    /// [`crate::coach_hints::MAX_SEEN`]); each shows once.
    #[serde(deserialize_with = "lenient_completed")]
    pub hints_seen: Vec<String>,
    /// "Also ask the assistant" in the Ask the coach box (`coach.assist`, fork AI3.5). Off by
    /// default: a CPU answer takes about a minute (provisional until GPU inference).
    #[serde(deserialize_with = "lenient_off")]
    pub assistant: bool,
}

impl Default for CoachPrefs {
    fn default() -> Self {
        Self { hints: true, completed: Vec::new(), hints_seen: Vec::new(), assistant: false }
    }
}

impl CoachPrefs {
    /// Keep at most [`MAX_COMPLETED`] lesson ids (the newest), each a valid id, no repeats.
    pub fn clamp(&mut self) {
        self.completed = clean_ids(std::mem::take(&mut self.completed), MAX_COMPLETED);
        self.hints_seen = clean_ids(std::mem::take(&mut self.hints_seen), crate::coach_hints::MAX_SEEN);
    }
}

/// The newest `max` valid ids of `list`, without repeats, oldest first.
fn clean_ids(list: Vec<String>, max: usize) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for id in list {
        if valid_id(&id) && !seen.contains(&id) {
            seen.push(id);
        }
    }
    let extra = seen.len().saturating_sub(max);
    seen.drain(..extra);
    seen
}

/// A malformed `hints` (preferences files are hand-edited) resets only that field: a failed field
/// would otherwise fail the whole preferences file.
fn lenient_hints<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(Value::deserialize(d)?.as_bool().unwrap_or(true))
}

/// A malformed `assistant` is off.
fn lenient_off<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(Value::deserialize(d)?.as_bool().unwrap_or(false))
}

/// A malformed `completed` (or `hintsSeen`) keeps its string entries (or none) instead of failing the file.
fn lenient_completed<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(v.as_array().map(|a| a.iter().filter_map(Value::as_str).take(MAX_COMPLETED * 4).map(str::to_string).collect()).unwrap_or_default())
}

/// A lesson in progress.
#[derive(Clone, Debug)]
pub struct Run {
    pub lesson: Arc<Lesson>,
    /// Index of the current step.
    pub step: usize,
    /// Steps skipped so far (indices).
    pub skipped: Vec<usize>,
    /// A step reached with Back: never finished by its `already` condition.
    pub held: Option<usize>,
}

impl Run {
    pub fn current(&self) -> Option<&Step> {
        self.lesson.steps.get(self.step)
    }
}

/// The coach's state in the session.
#[derive(Clone, Debug, Default)]
pub struct Coach {
    pub active: Option<Run>,
    /// The lesson finished last (cleared by `coach.exit` or the next start), for the UI's
    /// "lesson complete" card.
    pub finished: Option<Arc<Lesson>>,
    /// The context hint on screen (fork G2.12 part 3, [`crate::coach_hints`]); never while a
    /// lesson runs.
    pub hint: Option<Arc<crate::coach_hints::Hint>>,
    /// Bumped on every change (the UI and agents can watch it).
    pub revision: u64,
}

/// What a step change did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advance {
    /// Nothing changed.
    Stay,
    /// Moved to the next step.
    Next,
    /// That was the last step: the lesson is finished.
    Finished,
}

impl Coach {
    pub fn start(&mut self, lesson: Arc<Lesson>) {
        self.active = Some(Run { lesson, step: 0, skipped: Vec::new(), held: None });
        self.finished = None;
        self.hint = None;
        self.revision = self.revision.wrapping_add(1);
    }

    /// The current step, if a lesson is running.
    pub fn step(&self) -> Option<&Step> {
        self.active.as_ref().and_then(Run::current)
    }

    fn forward(&mut self, skipped: bool) -> Advance {
        let Some(run) = self.active.as_mut() else { return Advance::Stay };
        if skipped && !run.skipped.contains(&run.step) {
            run.skipped.push(run.step);
        }
        run.step = run.step.saturating_add(1);
        run.held = None;
        self.revision = self.revision.wrapping_add(1);
        if run.step >= run.lesson.steps.len() {
            self.finished = self.active.take().map(|r| r.lesson);
            Advance::Finished
        } else {
            Advance::Next
        }
    }

    /// A command succeeded: advance when it is what the current step waits for.
    pub fn observe_command(&mut self, id: &str, params: &Value) -> Advance {
        let done = match self.step().map(|s| &s.expect) {
            Some(Expect::Command { ids, params: pattern }) => ids.iter().any(|w| w == id) && pattern.as_ref().is_none_or(|p| matches(p, params)),
            _ => false,
        };
        if done { self.forward(false) } else { Advance::Stay }
    }

    /// A UI element is on screen: advance when the current step waits for it.
    pub fn observe_element(&mut self, element: &str) -> Advance {
        let done = matches!(self.step().map(|s| &s.expect), Some(Expect::Element(e)) if e == element);
        if done { self.forward(false) } else { Advance::Stay }
    }

    /// Next: only a manual step finishes this way.
    pub fn press_next(&mut self) -> std::result::Result<Advance, String> {
        match self.step().map(|s| &s.expect) {
            None => Err("no lesson is running".into()),
            Some(Expect::Manual) => Ok(self.forward(false)),
            Some(_) => Err("this step finishes when you do it (or use Skip)".into()),
        }
    }

    pub fn skip(&mut self) -> std::result::Result<Advance, String> {
        if self.active.is_none() {
            return Err("no lesson is running".into());
        }
        Ok(self.forward(true))
    }

    pub fn back(&mut self) -> std::result::Result<(), String> {
        let run = self.active.as_mut().ok_or("no lesson is running")?;
        if run.step == 0 {
            return Err("this is the first step".into());
        }
        run.step -= 1;
        let s = run.step;
        run.skipped.retain(|x| *x != s);
        run.held = Some(s);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    pub fn exit(&mut self) {
        self.active = None;
        self.finished = None;
        self.revision = self.revision.wrapping_add(1);
    }

    /// The state for `coach.state` and the UI.
    pub fn to_json(&self) -> Value {
        let finished = self.finished.as_ref().map(|l| json!({"lesson": l.id, "title": l.title}));
        let Some(run) = &self.active else {
            return json!({"active": false, "finished": finished, "revision": self.revision});
        };
        let step = run.current();
        json!({
            "active": true,
            "lesson": run.lesson.id,
            "title": run.lesson.title,
            "provenance": run.lesson.provenance.iter().map(|p| p.tag()).collect::<Vec<_>>(),
            "step": run.step,
            "steps": run.lesson.steps.len(),
            "text": step.map(|s| s.text.as_str()),
            "why": step.map(|s| s.why.as_str()),
            "target": step.and_then(|s| s.targets.first()),
            "targets": step.map(|s| s.targets.as_slice()).unwrap_or_default(),
            "expect": step.map(|s| s.expect.to_json()),
            "already": step.and_then(|s| s.already),
            "skipped": run.skipped.len(),
            "finished": finished,
            "revision": self.revision,
        })
    }
}

/// Remember a finished lesson in the preferences (saved when the session persists them).
fn record_finished(s: &mut Session) {
    let Some(id) = s.coach.finished.as_ref().map(|l| l.id.clone()) else { return };
    let done = &mut s.prefs.color.coach.completed;
    if done.contains(&id) {
        return;
    }
    done.push(id);
    if done.len() > MAX_COMPLETED {
        let extra = done.len() - MAX_COMPLETED;
        done.drain(..extra);
    }
    if let Some(path) = s.prefs_path.clone()
        && let Err(e) = s.prefs.save(&path)
    {
        s.log.push(crate::panels::Level::Warning, "coach", format!("couldn't save the finished lesson: {e}"));
    }
}

fn after(s: &mut Session, a: Advance) {
    if a == Advance::Finished {
        record_finished(s);
    } else if a == Advance::Next {
        settle(s);
    }
}

/// Whether `a` holds now.
fn already_holds(s: &Session, a: Already) -> bool {
    match a {
        Already::ClipSelected => s.active_sequence().is_some_and(|q| s.state.selection.iter().any(|c| crate::grade::is_video(q, *c))),
    }
}

/// Finish the current step (and the ones after it) while its `already` condition holds; a step
/// reached with Back stays.
fn settle(s: &mut Session) {
    for _ in 0..MAX_STEPS {
        let Some(run) = &s.coach.active else { return };
        let Some(a) = run.current().and_then(|st| st.already) else { return };
        if run.held == Some(run.step) || !already_holds(s, a) {
            return;
        }
        let adv = s.coach.forward(false);
        if adv == Advance::Finished {
            record_finished(s);
            return;
        }
    }
}

/// Called by [`Session::execute`] after every successful top-level command (other than
/// `coach.*`): a running lesson watches it, otherwise the context hints do.
pub(crate) fn observe_command(s: &mut Session, id: &str, params: &Value) {
    if s.coach.active.is_none() {
        crate::coach_hints::observe_command(s, id, params);
        return;
    }
    let a = s.coach.observe_command(id, params);
    after(s, a);
}

impl Session {
    /// The UI reports a control that is on screen (an `element` step waits for it; a context
    /// hint may be shown the first time).
    pub fn coach_element_seen(&mut self, element: &str) {
        if self.coach.active.is_none() {
            crate::coach_hints::observe_element(self, element);
            return;
        }
        let a = self.coach.observe_element(element);
        after(self, a);
    }
}

// ---------- commands ----------

type RunFn = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: RunFn) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("coach.lessons", "List Lessons", "{}", lessons),
        spec("coach.start", "Start Lesson", r#"{"lesson":str}"#, start),
        spec("coach.state", "Lesson State", "{}", |s, _| {
            settle(s);
            Ok(s.coach.to_json())
        }),
        spec("coach.next", "Next Lesson Step", "{}", |s, _| {
            let a = s.coach.press_next().map_err(|m| bad("coach.next", m))?;
            after(s, a);
            Ok(s.coach.to_json())
        }),
        spec("coach.back", "Previous Lesson Step", "{}", |s, _| {
            s.coach.back().map_err(|m| bad("coach.back", m))?;
            Ok(s.coach.to_json())
        }),
        spec("coach.skip", "Skip Lesson Step", "{}", |s, _| {
            let a = s.coach.skip().map_err(|m| bad("coach.skip", m))?;
            after(s, a);
            Ok(s.coach.to_json())
        }),
        spec("coach.exit", "Exit Lesson", "{}", |s, _| {
            s.coach.exit();
            Ok(s.coach.to_json())
        }),
        spec("coach.seen", "Report Element On Screen", r#"{"element":str}"#, |s, p| {
            let e = str_p(p, "element").filter(|e| valid_id(e)).ok_or_else(|| bad("coach.seen", "need `element` (an automation id)"))?;
            s.coach_element_seen(e);
            Ok(s.coach.to_json())
        }),
    ]
}

fn lessons(s: &mut Session, _: &Value) -> Result<Value> {
    let done = &s.prefs.color.coach.completed;
    let list: Vec<Value> = bundled()
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "title": l.title,
                "summary": l.summary,
                "provenance": l.provenance.iter().map(|p| p.tag()).collect::<Vec<_>>(),
                "steps": l.steps.len(),
                "completed": done.contains(&l.id),
            })
        })
        .collect();
    Ok(json!({ "lessons": list }))
}

fn start(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "coach.start";
    let id = str_p(p, "lesson").ok_or_else(|| bad(CMD, "need `lesson` (see coach.lessons)"))?;
    let l = lesson(id).ok_or_else(|| bad(CMD, format!("no lesson `{}` (see coach.lessons)", id.chars().take(MAX_ID).collect::<String>())))?;
    s.coach.start(l);
    settle(s);
    Ok(s.coach.to_json())
}
