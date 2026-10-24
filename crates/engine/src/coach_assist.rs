//! The assistant's prompt (fork AI-PLAN AI3 item 2, step AI3.2b): the guide sections
//! [`coach_ask::retrieve`] finds for a question, the current grade's facts and what is on screen,
//! packed under a size budget for a local chat model. No model is run here.
//!
//! Each section goes in as its compact header and numbered steps first; the full text only for
//! the best one or two while the budget allows (AI-PLAN AI3 0d: ≈ 3–5× fewer prompt tokens). Every
//! section is labelled with its source id so the answer can cite it and the grounding check
//! (AI3.4) can tell guide steps from general advice.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use crate::ai_explain::ExplainBackend;
use crate::coach_ask::{self, MAX_QUESTION_CHARS, MAX_RETRIEVE, Retrieved};
use crate::commands::{CommandSpec, always, bad};
use crate::{Result, Session};

/// Default prompt budget in characters (≈ 1 100 tokens at ~4 characters a token): prefill on the CPU
/// runs at ~40 tokens/s for SmolLM3-3B on the reference PC (AI3.3a), so the prompt stays short.
pub const DEFAULT_BUDGET_CHARS: usize = 4_500;
/// Largest budget accepted, in characters.
pub const MAX_BUDGET_CHARS: usize = 64_000;
/// Sections retrieved for one question.
pub const SECTIONS: usize = 6;
/// How many of the best sections may go in with their full text.
pub const FULL_TEXT: usize = 1;
/// Longest grade facts / on-screen text taken, in characters (each field, and all of them together).
pub const MAX_CONTEXT_CHARS: usize = 3_000;
/// Longest full section text taken, in characters.
const MAX_BODY_CHARS: usize = 4_000;

const SYSTEM: &str = "You are Revolution's editing and grading assistant. \
Answer the user's question using the guide sections given. \
Revolution steps, buttons, panels, menus and commands may come only from those sections; never invent one. \
Cite the sections you used by their id in square brackets, like [color.nodes.why]. \
Advice the sections don't state is allowed only on a line starting with \"General advice:\". \
If the sections don't cover the question, say you don't know and name the closest section. \
If a section says something is not in Revolution yet, say so and describe no steps for it. \
Name buttons, panels and shortcuts only as the sections write them. \
Give options with their trade-offs, not one right way. Keep it short and plain: at most 6 steps, no headings.";

/// What the user is looking at, when the question is asked (all optional).
#[derive(Clone, Debug, Default)]
pub struct AssistContext {
    /// The page or workspace, e.g. "Color".
    pub page: Option<String>,
    /// The selected node, clip or tool, in words.
    pub selection: Option<String>,
    /// The current grade's facts (the node explainer's JSON, G2.10).
    pub grade_facts_json: Option<String>,
    /// The frame under the playhead, measured (AI4.1b: `picture_facts` sheets before / after the grade).
    pub picture_facts: Option<String>,
    /// AI4.2: what the vision model saw in the frame, asked with the question (item 2b).
    pub picture_seen: Option<String>,
    /// AI4.2: the frame for the vision model to look at in the background (taken when asked).
    pub picture: Option<Arc<PictureInput>>,
    /// AI4.2b: the shot the frame belongs to (clip and its state), so a look is remembered for
    /// later questions about the same shot that don't ask for a fresh look.
    pub shot: Option<u64>,
    /// A follow-up: the question asked just before (AI3.5); retrieval reads both questions.
    pub previous_question: Option<String>,
    /// The assistant's checked answer to [`AssistContext::previous_question`].
    pub previous_answer: Option<String>,
    /// Free answers, or only the guide's own steps picked by the model (owner question Q35 (b)).
    pub style: AssistStyle,
    /// REF: the reference picture the question is about, measured against the shot, with
    /// FilmCraft's own two-part recipe ([`crate::coach_reference`]).
    pub reference: Option<Arc<crate::coach_reference::RefAnswer>>,
    /// REF.4: the reference picture for the vision model (taken when asked).
    pub reference_picture: Option<Arc<PictureInput>>,
    /// REF.4: what the vision model saw in the reference picture.
    pub reference_seen: Option<String>,
}

/// A small frame for the vision model: display RGB in 0..=1, row by row
/// ([`crate::grade_tools::program_frame_rgb`]).
#[derive(Clone)]
pub struct PictureInput {
    pub rgb: Vec<f32>,
    pub w: usize,
    pub h: usize,
}

impl std::fmt::Debug for PictureInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PictureInput({} × {})", self.w, self.h)
    }
}

/// Looks at a frame with the question in mind and says in a sentence or two what bears on it
/// (AI-PLAN AI4 items 2 / 2b). Blocking: called on the assistant's background thread.
pub trait PictureLooker: Send + Sync {
    fn look(&self, picture: &PictureInput, question: &str) -> std::result::Result<String, String>;
    /// Look with a prompt of the caller's own (REF.4: a reference picture is described with
    /// [`crate::coach_reference::reference_look_prompt`]). Lookers without a model of their own
    /// (tests) answer as [`PictureLooker::look`] would for the prompt as the question.
    fn look_with(&self, picture: &PictureInput, prompt: &str) -> std::result::Result<String, String> {
        self.look(picture, prompt)
    }
}

/// Pixels the frame is rendered at for the vision model (≈ 512 × 512: ~256 picture tokens).
pub const LOOK_PIXELS: u64 = 1 << 18;
/// Longest description kept from the vision model.
const MAX_SEEN_CHARS: usize = 600;
/// Looks remembered ([`AssistJobs`]).
const MAX_LOOKS: usize = 8;

/// The look cache's key: the frame's pixels (as 8-bit) and size, and the question.
fn look_key(p: &PictureInput, question: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (p.w, p.h, question.trim()).hash(&mut h);
    for v in &p.rgb {
        ((v.clamp(0.0, 1.0) * 255.0).round() as u8).hash(&mut h);
    }
    h.finish()
}

/// The vision model's instruction: what matters for grading, about this question, nothing made up.
pub fn look_prompt(question: &str) -> String {
    format!(
        "A colourist is asking about this video frame: \"{}\". In one or two plain sentences, describe only what you can see \
         that bears on that question (light sources and their colour and direction, skin, sky, practical lights, time of day, \
         interior or exterior, exposure). Don't give advice and don't guess what you can't see.",
        chars(question.trim(), MAX_QUESTION_CHARS)
    )
}

/// How the model answers (AI3.6, OWNER-QUESTIONS Q35).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AssistStyle {
    /// The model writes the answer; [`coach_ask::ground_with_text`] takes out what it invents.
    #[default]
    Free,
    /// The model only picks and orders the guide's numbered steps by key; FilmCraft writes the
    /// answer from the guide's own text, so nothing can be invented. Provisional (Q35 (b)): not
    /// the library default, but [`DEFAULT_STYLE`] for the command.
    Pick,
}

/// The style `coach.assist` (and so the Ask the coach box) uses when none is given: pick, since
/// the 85-case eval gave pick 69 / 85 auto-pass with nothing invented against free's 7 / 85
/// (2026-10-14, synthetic). Provisional (owner question Q35); `AssistStyle::default()` stays free
/// for library callers.
pub const DEFAULT_STYLE: AssistStyle = AssistStyle::Pick;

impl AssistStyle {
    /// From the `style` command parameter (`free` / `pick`).
    pub fn parse(s: &str) -> Option<AssistStyle> {
        match s {
            "free" => Some(AssistStyle::Free),
            "pick" => Some(AssistStyle::Pick),
            _ => None,
        }
    }
}

/// Most characters of the earlier question / answer a follow-up carries.
pub const MAX_PREVIOUS_CHARS: usize = 800;

/// A prompt ready for [`filmcraft_llm::chat_prompt`].
#[derive(Clone, Debug, PartialEq)]
pub struct AssistPrompt {
    pub system: String,
    pub user: String,
    /// The source ids sent, best first (a section without a header is `page#heading`).
    pub sources: Vec<String>,
    /// The guide text sent (what a named control must come from, [`coach_ask::ground_with_text`]).
    pub sent_text: String,
    /// When the best sections say FilmCraft doesn't have this yet: (section id, what they say).
    pub gap: Option<(String, String)>,
}

/// The source id of a retrieved section.
pub fn source_id(r: &Retrieved) -> String {
    match r.header {
        Some(h) => h.id.clone(),
        None => format!("{}#{}", r.section.page, r.section.heading),
    }
}

/// `s` cut to at most `max` characters.
fn chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A guide step cut to at most `max` characters at the end of a sentence or a table row, with " …"
/// where something was left out (a step cut mid-word read as a broken answer, AI3.6 lane E).
fn cut_step(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head = chars(s, max.saturating_sub(2));
    // after ". " or between two table rows ("… | | …"); not too early, else at a word
    let end = [head.rfind(". ").map(|i| i.saturating_add(1)), head.rfind("| |").map(|i| i.saturating_add(1))]
        .into_iter()
        .flatten()
        .max()
        .filter(|e| *e >= head.len() / 3)
        .or_else(|| head.rfind(' '))
        .unwrap_or(head.len());
    let mut out = head.get(..end).unwrap_or(head.as_str()).trim_end().to_string();
    out.push_str(" …");
    out
}

/// One section as the prompt carries it: id, heading, header line, numbered steps, and the full
/// text when `full`.
fn section_block(r: &Retrieved, full: bool) -> String {
    let mut out = format!("[{}] {} ({})\n", source_id(r), r.section.heading, r.section.page);
    if let Some(h) = &r.section.header {
        out.push_str(h.trim());
        out.push('\n');
    }
    let steps = coach_ask::numbered_list(&r.section.body);
    for (n, st) in steps.iter().take(coach_ask::MAX_STEPS).enumerate() {
        out.push_str(&format!("{}. {}\n", n.saturating_add(1), cut_step(st, coach_ask::MAX_STEP_TEXT)));
    }
    if full || steps.is_empty() {
        let body = if full { r.section.body.as_str() } else { r.section.body.split("\n\n").next().unwrap_or("") };
        out.push_str(&chars(body.trim(), MAX_BODY_CHARS));
        out.push('\n');
    }
    out
}

/// Most new tokens when the answer is "not yet, but here is the workaround".
pub const GAP_ANSWER_TOKENS: usize = 150;

/// What section `r` says FilmCraft lacks: its "**FilmCraft:** not yet …" line (practice notes)
/// or its "Planned: …" text (the guide's "later" sections), as plain text.
fn gap_line(r: &Retrieved) -> Option<String> {
    let body = r.section.body.as_str();
    let line = body.lines().map(str::trim).find(|l| {
        let l = l.trim_start_matches(['-', '*', ' ']).to_lowercase().replace("**", "");
        if l.starts_with("planned:") {
            return true;
        }
        // "Revolution: not yet …", "Revolution: no outside node yet; …", "… are not yet built"
        // ("FilmCraft:" in text written before the fork rebrand, FORK-PLAN §1a.103)
        let Some(rest) = l.strip_prefix("revolution:").or_else(|| l.strip_prefix("filmcraft:")) else { return false };
        let clause = rest.split([';', '.']).next().unwrap_or("");
        let words: Vec<&str> = clause.split(|c: char| !c.is_alphanumeric()).collect();
        words.contains(&"yet") && (words.contains(&"no") || words.contains(&"not"))
    })?;
    // the rest of that bullet (wrapped lines)
    let mut text = line.to_string();
    let rest = body.split(line).nth(1).unwrap_or("");
    for l in rest.lines().skip(1) {
        if l.trim().is_empty() || l.trim_start().starts_with('-') {
            break;
        }
        text.push(' ');
        text.push_str(l.trim());
    }
    Some(chars(text.replace("**", "").trim_start_matches(['-', ' ']), 400))
}

/// A section further down counts for the gap when it scores at least this share of the best.
const GAP_SCORE: f64 = 0.7;
/// How many of the best sections may state the gap.
const GAP_SECTIONS: usize = 1;

/// Words that say nothing about which feature a gap line is about.
const GAP_IGNORE: [&str; 7] = ["revolution", "filmcraft", "yet", "resolv", "premier", "plan", "use"];

/// Whether a gap line is about the question: they share a word (stemmed, stop words and
/// [`GAP_IGNORE`] left out). A section can then mark a gap in passing ("**FilmCraft:** no
/// Cut page yet") without telling every other question about it that a feature is missing.
/// How many of the gap clause's words the question must share at least: a third of a long
/// clause ("no subtract picker and no 3D / RGB / luma-only qualifier mode yet"), all of a
/// shorter one.
const GAP_WORDS: usize = 2;

/// Only the gap's own clause counts ("no Edge Thin yet" of "FilmCraft: no Edge Thin yet: use
/// Ultra Key's Choke"), not the workaround after it.
fn gap_matches(gap: &str, question: &str) -> bool {
    let rest = gap.split_once(':').map_or(gap, |(_, r)| r);
    let clause = rest.split([':', ';', '.']).next().unwrap_or(rest);
    let mut words: Vec<String> = coach_ask::tokenize(clause, 400).into_iter().filter(|w| !GAP_IGNORE.contains(&w.as_str())).collect();
    words.sort_unstable();
    words.dedup();
    // a bare "not yet (planned: …)" names nothing: the whole section is the gap
    let q = coach_ask::tokenize(question, MAX_QUESTION_CHARS);
    let need = GAP_WORDS.max(words.len().div_ceil(3)).min(words.len());
    words.iter().filter(|w| q.contains(w)).count() >= need
}

/// The gap the best section states, when its gap line is about the question.
fn gap_of(found: &[Retrieved], question: &str) -> Option<(String, String)> {
    let top = found.first()?.score;
    found
        .iter()
        .take(GAP_SECTIONS)
        .filter(|r| r.score >= GAP_SCORE * top)
        .find_map(|r| gap_line(r).filter(|g| gap_matches(g, question)).map(|g| (source_id(r), g)))
}

/// The prompt for `question`, at most `budget_chars` characters in all (clamped to
/// [`MAX_BUDGET_CHARS`]; the system text and question always go in). Sections that don't fit
/// are left out (a full text falls back to header + steps first).
pub fn assistant_prompt(question: &str, ctx: &AssistContext, budget_chars: usize) -> AssistPrompt {
    let budget = budget_chars.min(MAX_BUDGET_CHARS);
    let question = filmcraft_llm::sanitize(&chars(question.trim(), MAX_QUESTION_CHARS));
    let mut context = String::new();
    for (label, v) in [
        ("Page", &ctx.page),
        ("Selected", &ctx.selection),
        ("Current grade (facts)", &ctx.grade_facts_json),
        ("Picture (seen)", &ctx.picture_seen),
        ("Picture (measured)", &ctx.picture_facts),
        ("Reference picture (seen)", &ctx.reference_seen),
        ("Reference picture vs this shot (measured)", &ctx.reference.as_ref().map(|r| r.measured.join(" "))),
    ] {
        if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            context.push_str(&format!("{label}: {}\n", chars(v, MAX_CONTEXT_CHARS)));
        }
    }
    // (all of it together is capped too: a long selection must not crowd out the guide sections)
    let context = filmcraft_llm::sanitize(&chars(&context, MAX_CONTEXT_CHARS));
    let mut head = if context.is_empty() { String::new() } else { format!("On screen:\n{context}\n") };
    let prev = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(|v| filmcraft_llm::sanitize(&chars(v, MAX_PREVIOUS_CHARS)));
    let previous_question = prev(&ctx.previous_question);
    if let Some(pq) = &previous_question {
        head.push_str(&format!("Earlier in this conversation:\nQuestion: {pq}\n"));
        if let Some(pa) = prev(&ctx.previous_answer) {
            head.push_str(&format!("Your answer: {pa}\n"));
        }
        head.push_str("The new question may follow up on it.\n\n");
    }
    if let Some(r) = &ctx.reference {
        head.push_str(&filmcraft_llm::sanitize(&format!(
            "A reference picture is attached. Answer in two parts, \"In the grade\" and \"On set\", using this measured recipe:\n{}\
             Never claim to know exactly how the reference was made.\n\n",
            chars(&r.text(), MAX_CONTEXT_CHARS)
        )));
    }
    let tail = format!("Question: {question}");
    let fixed = SYSTEM.chars().count().saturating_add(head.chars().count()).saturating_add(tail.chars().count()).saturating_add("Guide sections:\n\n".len());
    let mut left = budget.saturating_sub(fixed);
    let mut blocks = String::new();
    let mut sources = Vec::new();
    // a follow-up ("and how do I undo that?") is about the earlier question's topic too
    let mut query = previous_question.as_ref().map_or_else(|| question.clone(), |pq| format!("{pq} {question}"));
    // (REF: the recipe's steps steer retrieval to the sections they come from)
    if let Some(r) = &ctx.reference {
        for st in r.grade.iter().skip(1) {
            query.push(' ');
            query.push_str(st.text);
        }
    }
    let mut found = retrieve_with_picture(&query, &question, ctx.picture_facts.as_deref(), SECTIONS.min(MAX_RETRIEVE));
    pin_recipe(&mut found, ctx);
    let gap = gap_of(&found, &query);
    let gap_note = gap.as_ref().map(|(id, g)| {
        filmcraft_llm::sanitize(&format!(
            "Important, what Revolution doesn't have yet [{id}]: {g}\nSay this first, then give only the workaround the sections describe. Describe no steps for what is missing.\n\n"
        ))
    });
    left = left.saturating_sub(gap_note.as_ref().map_or(0, |n| n.chars().count()));
    for (i, r) in found.iter().enumerate() {
        let tries = if i < FULL_TEXT { [true, false].as_slice() } else { [false].as_slice() };
        for &full in tries {
            let b = filmcraft_llm::sanitize(&section_block(r, full));
            let n = b.chars().count().saturating_add(1);
            if n <= left {
                left = left.saturating_sub(n);
                blocks.push_str(&b);
                blocks.push('\n');
                sources.push(source_id(r));
                break;
            }
        }
    }
    let note = gap_note.unwrap_or_default();
    let user =
        if sources.is_empty() { format!("{head}Guide sections: none found.\n\n{tail}") } else { format!("{head}Guide sections:\n{blocks}\n{note}{tail}") };
    AssistPrompt { system: SYSTEM.to_string(), user, sources, sent_text: blocks, gap }
}

// ---------- pick-the-steps answers (AI3.6, Q35 (b)) ----------

const PICK_SYSTEM: &str = "You are Revolution's editing and grading assistant. \
You answer only by choosing steps from the guide sections given. Every step has a key such as A1 or B3. \
Reply with the keys of the steps that answer the question, in the order to do them, at most 6, separated by spaces. \
A step that answers a yes / no question by saying what Revolution does counts as an answer. \
Write nothing else. If no step answers the question, reply NONE.";

/// Most new tokens for a list of step keys.
pub const PICK_ANSWER_TOKENS: usize = 40;
/// Most steps offered per section, and most steps picked.
pub const PICK_STEPS: usize = 8;
/// Most steps an answer keeps.
pub const PICK_MAX: usize = 6;
/// Picked steps from a section scoring below this share of the best section's retrieval score are
/// dropped (unless that leaves nothing): SmolLM3-3B pads answers with runs of keys from unrelated
/// sections. 0.5 dropped 39 padded lines of the 85-case eval and no expected source
/// (docs/fork/coach-eval/AI3-MUST-READ.md); 0.6 already lost useful steps.
pub const PICK_MIN_RELEVANCE: f64 = 0.5;

/// One step the model may pick.
#[derive(Clone, Debug, PartialEq)]
pub struct PickStep {
    /// "A1", "B3": section letter, step number.
    pub key: String,
    /// The section it comes from.
    pub source: String,
    /// The guide's text of the step.
    pub text: String,
    /// Its section's retrieval score as a share of the best section's (0..=1).
    pub relevance: f64,
    /// A step of a numbered procedure (a recipe), not a bullet or a paragraph.
    pub numbered: bool,
}

/// A pick-the-steps prompt: like [`AssistPrompt`], plus the keyed steps.
#[derive(Clone, Debug, PartialEq)]
pub struct PickPrompt {
    pub prompt: AssistPrompt,
    pub steps: Vec<PickStep>,
}

/// The steps a section offers: its numbered steps (true), or its first paragraph as one step
/// (or one per bullet).
fn section_steps(r: &Retrieved) -> (Vec<String>, bool) {
    let mut steps = coach_ask::numbered_list(&r.section.body);
    let numbered = !steps.is_empty();
    if steps.is_empty() {
        let para = r.section.body.split("\n\n").map(str::trim).find(|p| !p.is_empty() && !p.starts_with('<') && !p.starts_with('#')).unwrap_or("");
        if para.starts_with("- ") || para.starts_with("* ") {
            // a bullet list: one step per bullet (wrapped lines joined)
            for l in para.lines().map(str::trim) {
                match l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                    Some(item) => steps.push(item.to_string()),
                    None => {
                        if let Some(last) = steps.last_mut() {
                            last.push(' ');
                            last.push_str(l);
                        }
                    }
                }
            }
        } else if !para.is_empty() {
            steps.push(para.to_string());
        }
    }
    let steps =
        steps.into_iter().take(PICK_STEPS).map(|s| cut_step(s.split_whitespace().collect::<Vec<_>>().join(" ").as_str(), coach_ask::MAX_STEP_TEXT)).collect();
    (steps, numbered)
}

/// The best section is a clear winner when its retrieval score is at least this many times every
/// other section's. A reply that picks nothing from a clear winner gets the winner's steps instead
/// ([`answer_picks`]): the small model otherwise prefers a familiar weaker recipe (the owner's "lamp
/// light blue, room natural" got the whole-picture cooling steps instead of the one-light recipe).
/// Offering the winner alone was tried: the model then named keys of sections it wasn't given.
pub const DOMINANT_RATIO: f64 = 1.35;

/// Sections that push the picture one way in colour; a question that clearly wants the other way
/// never gets them (the owner's "make the light blue" follow-up once got the warm recipe's "towards
/// orange" steps, because it said "less orange").
const WARMING_SECTIONS: [&str; 1] = ["color.recipes.warm"];
const COOLING_SECTIONS: [&str; 2] = ["color.recipes.cool", "color.recipes.warm-to-blue"];
const WARM_WORDS: [&str; 8] = ["orange", "warm", "warmer", "warmth", "amber", "golden", "yellow", "tungsten"];
const COOL_WORDS: [&str; 7] = ["blue", "bluer", "cool", "cooler", "colder", "cold", "teal"];
/// A colour word right after one of these is one the asker wants less of ("less orange", "too
/// warm"). "not" / "no" / "nowhere" are left out: "not blue enough", "nowhere near blue" want more.
const LESS_WORDS: [&str; 5] = ["less", "too", "without", "reduce", "remove"];

/// Which way in colour `text` wants to go: `Some(true)` cooler, `Some(false)` warmer, `None` when
/// it doesn't say or says both equally.
fn wants_cooler(text: &str) -> Option<bool> {
    let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).take(400).collect();
    let (mut cool, mut warm) = (0_i32, 0_i32);
    for (i, w) in words.iter().enumerate() {
        let less = i.checked_sub(1).and_then(|j| words.get(j)).is_some_and(|p| LESS_WORDS.contains(&p.as_str()));
        if COOL_WORDS.contains(&w.as_str()) {
            if less { warm = warm.saturating_add(1) } else { cool = cool.saturating_add(1) }
        } else if WARM_WORDS.contains(&w.as_str()) {
            if less { cool = cool.saturating_add(1) } else { warm = warm.saturating_add(1) }
        }
    }
    match cool.cmp(&warm) {
        std::cmp::Ordering::Greater => Some(true),
        std::cmp::Ordering::Less => Some(false),
        std::cmp::Ordering::Equal => None,
    }
}

/// Whether section `id` pushes the picture against the way `query` wants to go.
fn against_the_asked_colour(id: &str, query: &str) -> bool {
    match wants_cooler(query) {
        Some(true) => WARMING_SECTIONS.contains(&id),
        Some(false) => COOLING_SECTIONS.contains(&id),
        None => false,
    }
}

/// The pick-the-steps prompt for `question` (same retrieval, context and gap note as
/// [`assistant_prompt`]), at most `budget_chars` characters; a section that doesn't fit, or whose picks
/// [`parse_picks`] would drop (relevance below [`PICK_MIN_RELEVANCE`]), is left out.
pub fn pick_prompt(question: &str, ctx: &AssistContext, budget_chars: usize) -> PickPrompt {
    let free = assistant_prompt(question, ctx, budget_chars);
    let budget = budget_chars.min(MAX_BUDGET_CHARS);
    let query = ctx.previous_question.as_deref().map(str::trim).filter(|v| !v.is_empty()).map_or_else(
        || filmcraft_llm::sanitize(&chars(question.trim(), MAX_QUESTION_CHARS)),
        |pq| filmcraft_llm::sanitize(&format!("{} {}", chars(pq, MAX_PREVIOUS_CHARS), chars(question.trim(), MAX_QUESTION_CHARS))),
    );
    let mut found = retrieve_with_picture(&query, question, ctx.picture_facts.as_deref(), SECTIONS.min(MAX_RETRIEVE));
    pin_recipe(&mut found, ctx);
    // the free prompt's head (context, earlier exchange) and tail (gap note, question) are reused
    let (head, rest) = free.user.split_once("Guide sections:").unwrap_or(("", free.user.as_str()));
    let asked = rest.rfind("Question: ").and_then(|i| rest.get(i..)).unwrap_or("");
    let tail = match &free.gap {
        Some((id, g)) => filmcraft_llm::sanitize(&format!(
            "Note, Revolution doesn't have this yet [{id}]: {g}\nPick only steps of the workaround the sections describe, or NONE.\n\n{asked}"
        )),
        None => asked.to_string(),
    };
    let fixed =
        PICK_SYSTEM.chars().count().saturating_add(head.chars().count()).saturating_add(tail.chars().count()).saturating_add("Guide sections:\n\n".len());
    let mut left = budget.saturating_sub(fixed);
    let (mut blocks, mut sources, mut steps) = (String::new(), Vec::new(), Vec::new());
    let letters = ('A'..='Z').take(SECTIONS);
    let best = found.iter().map(|r| r.score).fold(0.0_f64, f64::max);
    for (r, letter) in found.iter().zip(letters) {
        let id = source_id(r);
        let (texts, numbered) = section_steps(r);
        let relevance = if best > 0.0 && r.score.is_finite() { (r.score / best).clamp(0.0, 1.0) } else { 1.0 };
        // a section whose picks `parse_picks` would drop only costs prefill time; one pushing the
        // colour against what was asked would contradict the answer
        if texts.is_empty() || relevance < PICK_MIN_RELEVANCE || against_the_asked_colour(&id, &query) {
            continue;
        }
        let mut b = format!("[{id}] {} ({})\n", r.section.heading, r.section.page);
        let mut keyed = Vec::new();
        for (n, t) in texts.iter().enumerate() {
            let key = format!("{letter}{}", n.saturating_add(1));
            b.push_str(&format!("{key}: {t}\n"));
            keyed.push(PickStep { key, source: id.clone(), text: t.clone(), relevance, numbered });
        }
        let b = filmcraft_llm::sanitize(&b);
        let n = b.chars().count().saturating_add(1);
        if n > left {
            // the best section comes anyway, with as many of its steps as fit (a long on-screen
            // context must not swap it for a short, weaker one); others that don't fit are left out
            if !sources.is_empty() {
                continue;
            }
            let mut cut = format!("[{id}] {} ({})\n", r.section.heading, r.section.page);
            let mut kept = Vec::new();
            for k in keyed {
                let line = format!("{}: {}\n", k.key, k.text);
                if filmcraft_llm::sanitize(&format!("{cut}{line}")).chars().count().saturating_add(1) > left {
                    break;
                }
                cut.push_str(&line);
                kept.push(k);
            }
            if kept.is_empty() {
                continue;
            }
            let cut = filmcraft_llm::sanitize(&cut);
            left = left.saturating_sub(cut.chars().count().saturating_add(1));
            blocks.push_str(&cut);
            blocks.push('\n');
            sources.push(id);
            steps.extend(kept);
            continue;
        }
        left = left.saturating_sub(n);
        blocks.push_str(&b);
        blocks.push('\n');
        sources.push(id);
        steps.extend(keyed);
    }
    let user = if sources.is_empty() { format!("{head}Guide sections: none found.\n\n{tail}") } else { format!("{head}Guide sections:\n{blocks}\n{tail}") };
    PickPrompt { prompt: AssistPrompt { system: PICK_SYSTEM.to_string(), user, sources, sent_text: blocks, gap: free.gap }, steps }
}

/// The steps a model reply picks, in its order (unknown keys ignored, repeats once, steps below
/// [`PICK_MIN_RELEVANCE`] dropped while others remain, at most [`PICK_MAX`]); empty for "NONE" or
/// nothing usable.
pub fn parse_picks<'a>(reply: &str, steps: &'a [PickStep]) -> Vec<&'a PickStep> {
    let mut out = picked_keys(reply, steps);
    if out.iter().any(|s| s.relevance >= PICK_MIN_RELEVANCE) {
        out.retain(|s| s.relevance >= PICK_MIN_RELEVANCE);
    }
    out.truncate(PICK_MAX);
    out
}

/// Every known step a reply names, in its order, each once.
fn picked_keys<'a>(reply: &str, steps: &'a [PickStep]) -> Vec<&'a PickStep> {
    let mut out: Vec<&PickStep> = Vec::new();
    for word in reply.split(|c: char| !c.is_ascii_alphanumeric()).take(200) {
        let w = word.to_ascii_uppercase();
        if let Some(st) = steps.iter().find(|s| s.key == w)
            && !out.iter().any(|o| o.key == st.key)
        {
            out.push(st);
        }
    }
    out
}

/// Most lines a picked answer shows once [`answer_picks`] has completed its recipes and notes.
pub const PICK_ANSWER_MAX: usize = 10;
/// A picked section scoring at least this share of the best section is completed by
/// [`answer_picks`]; weaker ones (padding, mostly) keep only the steps picked.
pub const COMPLETE_MIN_RELEVANCE: f64 = 0.85;
/// When a section scoring at least [`COMPLETE_MIN_RELEVANCE`] was picked, picks from sections
/// below this share of the best go (padding from far weaker sections).
pub const KEEP_MIN_RELEVANCE: f64 = 0.55;
/// Sections about the app in general (the desktop, the hardware, words, keys, the coach itself):
/// picks from them are dropped when the best section is about something else.
/// (`getting-started` is not one: its import steps answered the input colour space of
/// `adv-match-cams`.)
const GENERAL_FAMILIES: [&str; 6] = ["linux", "hardware", "glossary", "shortcuts", "pages", "color.coach"];

/// The family of a section id: `color.coach` for the coach's page, else the page's first part.
fn family(id: &str) -> &str {
    if id.starts_with("color.coach.") || id == "color.coach" {
        return "color.coach";
    }
    id.split(['.', '#']).next().unwrap_or(id)
}

/// The guide's broad areas, by section id prefix (longest first wins): a pick from another area
/// than the best section's is padding unless it matches well (AI3.6 lane E: a relink answer
/// ending with glow and roll-off lines, a scene-look answer with voice-over takes).
const AREAS: [(&str, &str); 36] = [
    ("practice.grading", "grade"),
    ("practice.looks", "grade"),
    ("practice.node-trees", "grade"),
    ("practice.mixed-cameras", "grade"),
    ("practice.delivery", "deliver"),
    ("practice.youtube", "edit"),
    ("practice.editing", "edit"),
    ("practice.workflow", "edit"),
    ("practice.audio", "audio"),
    ("practice.titles", "comp"),
    ("color", "grade"),
    ("packs", "grade"),
    ("compositor", "comp"),
    ("keying", "comp"),
    ("tracking", "comp"),
    ("shader-effects", "comp"),
    ("graphics", "comp"),
    ("effects", "comp"),
    ("editing", "edit"),
    ("trimming", "edit"),
    ("timeline", "edit"),
    ("edit-menu", "edit"),
    ("text", "edit"),
    ("media", "edit"),
    ("file-menu", "edit"),
    ("command-reference", "edit"),
    ("cards", "edit"),
    ("projects", "edit"),
    ("audio", "audio"),
    ("essential-sound", "audio"),
    ("mixing", "audio"),
    ("export", "deliver"),
    ("export-queue", "deliver"),
    ("render-cache", "deliver"),
    ("proxies", "deliver"),
    ("hardware", "deliver"),
];
/// Areas whose sections often answer together (delivery settings sit beside grading and editing).
const NEAR_AREAS: [(&str, &str); 3] = [("grade", "deliver"), ("edit", "deliver"), ("edit", "audio")];

/// The broad area of a section id ([`AREAS`]); `None` for the general pages.
fn area(id: &str) -> Option<&'static str> {
    AREAS
        .iter()
        .filter(|(prefix, _)| id == *prefix || id.strip_prefix(prefix).is_some_and(|r| r.starts_with('.') || r.starts_with('#')))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, a)| *a)
}

/// Whether sections of areas `a` and `b` can answer one question together.
fn near(a: Option<&str>, b: Option<&str>) -> bool {
    a == b || NEAR_AREAS.iter().any(|(x, y)| (a == Some(*x) && b == Some(*y)) || (a == Some(*y) && b == Some(*x)))
}

/// A practice note bullet's label ("**Pitfalls:** …" → "pitfalls"), lower case.
fn note_label(text: &str) -> Option<String> {
    let t = text.trim_start().strip_prefix("**")?;
    let (label, _) = t.split_once(":**")?;
    (label.chars().count() <= 30).then(|| label.trim().to_lowercase())
}

/// Whether a note line only says how another app does it, or the goal: left out when the note is
/// completed (the model may still pick it).
fn side_line(text: &str) -> bool {
    note_label(text).is_some_and(|l| l == "goal" || l.contains("resolve") || l.contains("premiere"))
}

/// The section that is a clear winner among the offered steps ([`DOMINANT_RATIO`]), if any.
fn clear_winner(steps: &[PickStep]) -> Option<&str> {
    let winner = steps.iter().find(|s| s.relevance >= 1.0)?;
    let runner_up = steps.iter().filter(|s| s.source != winner.source).map(|s| s.relevance).fold(0.0_f64, f64::max);
    (runner_up.is_finite() && runner_up * DOMINANT_RATIO <= 1.0).then_some(winner.source.as_str())
}

/// The answer's steps for a model reply (AI3.6 lane E): [`parse_picks`], then
/// - picks from a general section (Linux, hardware, glossary, the coach's own page …) go when the
///   best section is of another family;
/// - when a section scoring at least [`COMPLETE_MIN_RELEVANCE`] was picked, picks from sections
///   below [`KEEP_MIN_RELEVANCE`] go, and so do picks below [`COMPLETE_MIN_RELEVANCE`] from an
///   area ([`AREAS`]) that doesn't answer with the best section's;
/// - a section scoring at least [`COMPLETE_MIN_RELEVANCE`], and the best-scoring section picked,
///   is completed: a recipe from the step before its first picked one to its end, in order
///   (answers used to stop a step short or start mid-recipe, or skip the step a pick builds on); a
///   practice note with its own lines (FilmCraft, Why, Pitfalls …, not how Resolve / Premiere do
///   it, not the goal); a plain bullet list whole. Completion only uses the lines the model's own
///   picks leave free.
///
/// At most [`PICK_ANSWER_MAX`] lines. (Dropping the "Goal:" / "Why:" lines of weak sections was
/// tried: it took out 4 lines of the 85-case run, one of them the only expected source.)
pub fn answer_picks<'a>(reply: &str, steps: &'a [PickStep]) -> Vec<&'a PickStep> {
    let mut picks = parse_picks(reply, steps);
    // a clear winner the reply ignored: its first step (completed below to the whole recipe)
    if let Some(first) = clear_winner(steps).and_then(|w| steps.iter().find(|s| s.source == w))
        && !picks.iter().any(|p| p.source == first.source)
    {
        picks = vec![first];
    }
    let best = steps.iter().find(|s| s.relevance >= 1.0).or(steps.first()).map(|s| family(&s.source));
    let general = |s: &PickStep| best.is_some_and(|b| family(&s.source) != b && GENERAL_FAMILIES.contains(&family(&s.source)));
    if picks.iter().any(|s| !general(s)) {
        picks.retain(|s| !general(s));
    }
    // far weaker sections' picks, and other areas' picks that don't match well, go when a
    // well-matching section was picked
    if picks.iter().any(|s| s.relevance >= COMPLETE_MIN_RELEVANCE) {
        let best_area = steps.iter().find(|s| s.relevance >= 1.0).or(steps.first()).map(|s| area(&s.source));
        let off =
            |s: &PickStep| s.relevance < KEEP_MIN_RELEVANCE || (s.relevance < COMPLETE_MIN_RELEVANCE && best_area.is_some_and(|b| !near(b, area(&s.source))));
        if picks.iter().any(|s| !off(s)) {
            picks.retain(|s| !off(s));
        }
    }
    let mut out: Vec<&PickStep> = Vec::new();
    let mut done: Vec<&str> = Vec::new();
    let picked = |s: &PickStep| picks.iter().any(|p| p.key == s.key);
    // the picks themselves always fit; completion only uses the lines left over
    let mut spare = PICK_ANSWER_MAX.saturating_sub(picks.len().min(PICK_ANSWER_MAX));
    // the best-matching section picked is completed too, however it scores
    let top = picks.iter().map(|s| s.relevance).filter(|r| r.is_finite()).fold(f64::NEG_INFINITY, f64::max);
    for p in &picks {
        if out.iter().any(|o| o.key == p.key) {
            continue;
        }
        let strong = p.relevance >= COMPLETE_MIN_RELEVANCE || p.relevance >= top;
        if !strong || done.contains(&p.source.as_str()) {
            if !done.contains(&p.source.as_str()) {
                out.push(p);
            }
            continue;
        }
        done.push(&p.source);
        let section: Vec<&PickStep> = steps.iter().filter(|s| s.source == p.source).collect();
        let more: Vec<&PickStep> = if p.numbered {
            // from the step before the first one picked: a step builds on the one before it
            let first = section.iter().position(|s| picked(s)).unwrap_or(0);
            section.into_iter().skip(first.saturating_sub(1)).collect()
        } else {
            section.into_iter().filter(|s| picked(s) || !side_line(&s.text)).collect()
        };
        for s in more {
            if out.iter().any(|o| o.key == s.key) {
                continue;
            }
            if picked(s) {
                out.push(s);
            } else if spare > 0 {
                spare = spare.saturating_sub(1);
                out.push(s);
            }
        }
    }
    out.truncate(PICK_ANSWER_MAX);
    out
}

/// The first letters of a line, lower case, without its step number, punctuation or link
/// targets: two lines with the same start say the same thing ("FilmCraft: not yet (planned:
/// [color/nodes.md](…))" and "2. FilmCraft: not yet (planned: color/nodes.md …").
fn same_start(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    // "[label](target)" → "label"
    while let Some(i) = rest.find("](") {
        plain.push_str(rest.get(..i).unwrap_or(""));
        rest = rest.get(i.saturating_add(2)..).unwrap_or("");
        rest = rest.find(')').and_then(|j| rest.get(j.saturating_add(1)..)).unwrap_or("");
    }
    plain.push_str(rest);
    plain.chars().filter(|c| c.is_alphanumeric()).skip_while(|c| c.is_ascii_digit()).flat_map(char::to_lowercase).take(24).collect()
}

/// The answer text the picked steps make: one guide step per line with its section's citation.
fn picked_text(picks: &[&PickStep]) -> String {
    picks.iter().enumerate().map(|(n, s)| format!("{}. {} [{}]", n.saturating_add(1), s.text, s.source)).collect::<Vec<_>>().join("\n")
}

// ---------- answering (AI3.3c) ----------

/// Most new tokens of an assistant answer (~6 tokens/s on the reference PC: about half a minute).
pub const MAX_ANSWER_TOKENS: usize = 200;
/// Time for one assistant answer, prompt included.
pub const ANSWER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

/// Why the keyword coach answered instead of the model.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "reason")]
pub enum AssistFallback {
    /// The model can't run now (not installed, no memory, this build, …).
    NotReady(String),
    /// Loading or generating failed.
    Error(String),
    /// Nothing of the model's text survived the grounding check.
    Ungrounded,
    /// No guide section matched the question, so the model wasn't asked: "I don't know" beats a
    /// guess (AI-PLAN AI3 item 4).
    NothingFound,
}

/// The assistant's answer: the model's grounded text, or the keyword coach's answer and why.
#[derive(Clone, Debug, Default)]
pub struct AssistAnswer {
    /// The sections the model was given.
    pub sources: Vec<String>,
    /// The model's answer after [`coach_ask::ground`] (None when the coach answered).
    pub grounded: Option<coach_ask::Grounded>,
    /// Set when the keyword coach answered instead.
    pub fallback: Option<AssistFallback>,
    /// The model said the sections don't answer the question (its lines then name the closest
    /// section; the keyword coach's answer comes along in `coach`).
    pub dont_know: bool,
    /// The lesson that shows the model's answer ("Show me"), as (id, title).
    pub lesson: Option<(String, String)>,
    pub coach: Option<coach_ask::Answer>,
    /// AI4.1d: measured facts of the frame that fit the question ([`picture_note`]), shown as
    /// "On this frame (measured)"; written by FilmCraft, never by the model.
    pub seen: Vec<String>,
    /// REF: the reference answer (measured difference, recipe, on set, Do this), written by
    /// FilmCraft, and what the vision model saw in the reference picture.
    pub reference: Option<Arc<crate::coach_reference::RefAnswer>>,
    pub reference_seen: Option<String>,
}

/// Most lines of [`picture_note`].
pub const MAX_SEEN: usize = 3;

/// AI4.1d (provisional, owner question Q40): the measured facts of `sheet`
/// ([`filmcraft_render::picture_facts`] text, as `current_picture_facts` writes it) that fit
/// `question`, for the answer's "On this frame (measured)" lines. FilmCraft writes them, not the
/// model: a pick answer only chooses guide steps, and a fact the model rewrote could be wrong.
/// The current state (after the grade) is used; a general "fix my shot" question gets exposure
/// and white balance. Empty when nothing fits or there is no sheet.
pub fn picture_note(question: &str, sheet: Option<&str>) -> Vec<String> {
    const TOPICS: [(&str, &[&str]); 5] = [
        (
            "Exposure:",
            &[
                "dark",
                "bright",
                "expos",
                "clipped",
                "clipping",
                "blown",
                "highlight",
                "shadow",
                "black",
                "crush",
                "contrast",
                "flat",
                "underexp",
                "overexp",
                "too light",
                "white sky",
            ],
        ),
        (
            "White balance:",
            &[
                "white balance",
                "wb",
                "warm",
                "cool",
                "cold",
                "blue",
                "orange",
                "yellow",
                "green",
                "magenta",
                "tint",
                "cast",
                "temperature",
                "neutral",
                "kelvin",
            ],
        ),
        ("Saturation:", &["saturat", "vivid", "muted", "colourful", "colorful", "washed", "dull", "punch"]),
        ("Skin-tone colours", &["skin", "face", "faces", "people", "person", "portrait", "interview"]),
        ("clip \"", &["log", "f-log", "flog", "slog", "v-log", "input transform"]),
    ];
    const GENERAL: [&str; 7] = ["fix", "correct", "balance", "look right", "look better", "what's wrong", "what is wrong"];
    let Some(sheet) = sheet else { return Vec::new() };
    let q = format!(" {} ", question.to_lowercase().replace('\u{2019}', "'"));
    let has = |w: &str| {
        // whole-word for short words ("wb", "log", "cool"), substring for stems
        if w.len() <= 4 { q.split(|c: char| !c.is_alphanumeric() && c != '-').any(|t| t == w) } else { q.contains(w) }
    };
    let mut wanted: Vec<&str> = TOPICS.iter().filter(|(_, ws)| ws.iter().any(|w| has(w))).map(|(p, _)| *p).collect();
    if wanted.is_empty() && GENERAL.iter().any(|w| has(w)) {
        wanted = vec!["Exposure:", "White balance:"];
    }
    // the current state: the block after the grade when there is one
    let current = sheet.rsplit_once("after the grade:\n").map_or(sheet, |(_, a)| a);
    let mut out = Vec::new();
    for p in wanted {
        let line = if p.starts_with("clip") {
            sheet
                .lines()
                .next()
                .filter(|l| l.starts_with(p))
                .and_then(|l| l.split_once(" (source "))
                .map(|(_, r)| format!("Source: {}", r.split_once("),").map_or(r, |(a, _)| a).trim_end_matches(')')))
        } else {
            current.lines().find(|l| l.starts_with(p)).map(str::to_string)
        };
        if let Some(l) = line.filter(|l| !out.contains(l)) {
            out.push(chars(&l, 300));
        }
        if out.len() >= MAX_SEEN {
            break;
        }
    }
    out
}

/// Most problem phrases [`picture_terms`] adds to a retrieval query.
pub const MAX_PICTURE_TERMS: usize = 3;

/// AI4.5a: the frame's clear problems in `sheet` (the current state, as [`picture_note`] reads it)
/// as guide words ("warm cast white balance", "clipped highlights", …), worst first, at most
/// [`MAX_PICTURE_TERMS`], for the retrieval query: a picture question is often vague ("the client
/// says it's muddy") and the fact sheet names what the guide sections are about. Small deviations
/// add nothing; no sheet or an unreadable one gives an empty string.
pub fn picture_terms(sheet: Option<&str>) -> String {
    let Some(sheet) = sheet else { return String::new() };
    let current = sheet.rsplit_once("after the grade:\n").map_or(sheet, |(_, a)| a);
    let line = |p: &str| current.lines().find(|l| l.starts_with(p)).unwrap_or("");
    // the number right before `unit` in `l` ("2.1 stops", "18% clipped")
    let num_before = |l: &str, unit: &str| -> f32 {
        l.find(unit)
            .and_then(|i| l.get(..i))
            .and_then(|h| h.trim_end().rsplit(|c: char| c.is_whitespace() || c == '(').next())
            .and_then(|n| n.trim_end_matches('%').parse::<f32>().ok())
            .filter(|n| n.is_finite())
            .unwrap_or(0.0)
    };
    // (severity, words)
    let mut found: Vec<(f32, &str)> = Vec::new();
    let exp = line("Exposure:");
    let clipped = num_before(exp, "% clipped");
    let crushed = num_before(exp, "% crushed");
    let mids = num_before(exp, " stops above") - num_before(exp, " stops below");
    let contrast = exp.split_once("contrast").map_or(0.0, |(_, r)| num_before(r, " stops"));
    if clipped >= 2.0 {
        found.push((clipped / 2.0, "clipped highlights soft clip"));
    }
    if crushed >= 2.0 {
        found.push((crushed / 2.0, "crushed blacks black point"));
    }
    if mids <= -1.0 {
        found.push((-mids * 3.0, "dark underexposed raise exposure"));
    } else if mids >= 1.0 {
        found.push((mids * 3.0, "bright overexposed lower exposure"));
    }
    if contrast > 0.0 && contrast < 3.0 {
        found.push((3.0 - contrast + 1.0, "flat low contrast"));
    }
    let wb = line("White balance:");
    let cast = wb.split_once(" look ").map_or("", |(_, r)| r);
    let strength = if cast.starts_with("slightly") {
        1.0
    } else if cast.starts_with("very") {
        4.0
    } else {
        2.5
    };
    if cast.contains("warm") {
        found.push((strength, "warm orange cast white balance"));
    } else if cast.contains("cool") {
        found.push((strength, "cool blue cast white balance"));
    }
    if cast.contains("green") || cast.contains("magenta") {
        found.push((strength, "green magenta tint white balance"));
    }
    let sat = line("Saturation:");
    if sat.contains("low") {
        found.push((1.5, "muted saturation"));
    } else if sat.contains("high") {
        found.push((1.5, "oversaturated saturation"));
    }
    let skin = line("Skin-tone colours");
    if skin.contains("° towards") && num_before(skin, "°") >= 5.0 {
        found.push((num_before(skin, "°") / 5.0, "skin tone skin line"));
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    found.iter().take(MAX_PICTURE_TERMS).map(|(_, w)| *w).collect::<Vec<_>>().join(" ")
}

/// Sections of the plain retrieval [`retrieve_with_picture`] always keeps first.
const PICTURE_KEEP: usize = 3;

/// Whether `question` talks about the frame on screen ("this shot", "it's muddy", "faces look
/// purple"), so the measured facts may say what it is about.
fn asks_about_the_frame(question: &str) -> bool {
    const FRAME: [&str; 15] =
        ["this", "shot", "here", "frame", "footage", "picture", "image", "looks", "look", "it's", "he's", "she's", "muddy", "lifeless", "wrong"];
    let l = question.to_lowercase().replace('\u{2019}', "'");
    let mut words = l.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty());
    words.any(|w| FRAME.contains(&w)) || !picture_note(question, Some("Exposure:\nWhite balance:\nSaturation:\nSkin-tone colours")).is_empty()
}

/// AI4.5a (provisional, measured on the AI-POOL picture questions: top-5 hits 3 → 8 / 10; with an
/// unrelated sheet attached the other pool questions keep 766–771 of 781): [`coach_ask::retrieve`]
/// for `query`, and when `question` is about the frame and `sheet` names clear problems
/// ([`picture_terms`]), the plain result's best [`PICTURE_KEEP`] followed by what the query plus
/// those words finds, then the rest of the plain result; `k` sections at most.
///
/// AI4.5c.2: a question that uses creative words ([`TECHNIQUES`]) also gets the best section for
/// the techniques they call for, after the first [`SUB_GOAL_KEEP`]; not when the fact sheet
/// already steered the result (that would push its sections out). Splitting the question into
/// sub-goals is off ([`SPLIT_SUB_GOALS`]).
pub fn retrieve_with_picture(query: &str, question: &str, sheet: Option<&str>, k: usize) -> Vec<coach_ask::Retrieved> {
    retrieve_with_picture_in(crate::coach_meaning::default_mode(), query, question, sheet, k)
}

/// [`retrieve_with_picture`] with a chosen ranking ([`coach_ask::retrieve_in`]; fork §1a.98).
pub fn retrieve_with_picture_in(
    mode: crate::coach_meaning::SearchMode,
    query: &str,
    question: &str,
    sheet: Option<&str>,
    k: usize,
) -> Vec<coach_ask::Retrieved> {
    let plain = coach_ask::retrieve_in(mode, query, k);
    let terms = picture_terms(sheet);
    if terms.is_empty() || !asks_about_the_frame(question) {
        return with_sub_goals(mode, plain, question, k);
    }
    let mut seen = coach_ask::retrieve_in(mode, &format!("{query} {terms}"), k);
    // the longer query scores higher: put its scores on the plain query's scale, or the pick
    // prompt's relevance filter (a share of the best score) drops the plain query's own best
    // sections (the Ask box's "fix skin with a qualifier" lost the skin recipe this way)
    let best = |v: &[coach_ask::Retrieved]| v.iter().map(|r| r.score).filter(|x| x.is_finite()).fold(0.0_f64, f64::max);
    let (pb, sb) = (best(&plain), best(&seen));
    if pb > 0.0 && sb > 0.0 {
        for r in &mut seen {
            r.score *= pb / sb;
        }
    }
    let mut out: Vec<coach_ask::Retrieved> = plain.iter().take(PICTURE_KEEP).cloned().collect();
    for r in seen.into_iter().chain(plain.into_iter().skip(PICTURE_KEEP)) {
        if out.len() >= k {
            break;
        }
        if !out.iter().any(|o| std::ptr::eq(o.section, r.section)) {
            out.push(r);
        }
    }
    out
}

/// Sections of the whole-question result kept first when sub-goals add theirs.
const SUB_GOAL_KEEP: usize = 3;
/// Whether a question is split into [`sub_goals`] for retrieval. Off since 2026-10-16: on the
/// creative held-out set (646 plan sections in the pool's `creative-*` batches) the split cost
/// coverage (305 with it, 310 without; the 2026-10-15 gain, 6 → 8 / 21, was on the one batch it was
/// tuned on), while the [`TECHNIQUES`] lookup on the whole question still pays (298 without it).
/// Lookup questions: 821 hits either way.
const SPLIT_SUB_GOALS: bool = false;
/// At most this many sub-goals are looked up (bounded work for a long, hostile question).
const MAX_SUB_GOALS: usize = 6;

/// The parts of `question` that ask for different things, split at "and", "then", "with", "so",
/// "but", "instead" and punctuation; parts with fewer than two words of three letters or more
/// are dropped. Fewer than two parts → empty (a single goal needs no splitting).
pub fn sub_goals(question: &str) -> Vec<String> {
    const JOINS: [&str; 7] = [" and ", " then ", " with ", " so ", " but ", " instead ", " while "];
    let mut text = format!(" {} ", chars(question, MAX_QUESTION_CHARS).to_lowercase());
    for c in [',', ';', '.', '?', '!', ':'] {
        text = text.replace(c, " | ");
    }
    for j in JOINS {
        text = text.replace(j, " | ");
    }
    let parts: Vec<String> = text
        .split('|')
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| p.split(' ').filter(|w| w.chars().filter(|c| c.is_alphanumeric()).count() >= 3).count() >= 2)
        .take(MAX_SUB_GOALS)
        .collect();
    if parts.len() < 2 { Vec::new() } else { parts }
}

/// Creative words (what a shot should look like) → the techniques the guide names for them, used
/// only for sub-goal lookups (AI4.5c.2, provisional: measured on `pool/creative-1.toml`; later
/// creative batches are the honest check). Each side is matched as whole lowercase words.
const TECHNIQUES: &[(&str, &str)] = &[
    ("ghost translucent transparent see-through faded", "opacity blend mode merge"),
    ("remove erase person object", "mask track"),
    ("shake earthquake wobble jitter handheld", "keyframes position motion"),
    ("freeze frozen", "frame hold"),
    ("drain desaturate colourless colorless grey gray", "saturation keyframes"),
    ("clone twice twins double", "mask merge comp"),
    ("night", "day for night"),
    ("stormy storm clouds overcast", "sky darken"),
    ("screen monitor tv", "corner pin track surface"),
    ("subtitle subtitles word words karaoke", "captions transcript"),
    ("glow dreamy", "blur"),
];

/// `goal` plus the [`TECHNIQUES`] its words call for.
fn with_techniques(goal: &str) -> String {
    let mut words: HashSet<&str> = goal.split(|c: char| !c.is_alphanumeric() && c != '-').collect();
    // (a green / blue screen is keying, not a screen in the shot; the AI3 eval lost
    // `adv-comp-key-over-bg` to the screen-replacement recipe)
    if goal.contains("green screen") || goal.contains("blue screen") {
        words.remove("screen");
    }
    let mut out = goal.to_string();
    for (when, add) in TECHNIQUES {
        if when.split(' ').any(|w| words.contains(w)) {
            out.push(' ');
            out.push_str(add);
        }
    }
    out
}

/// `found` with each sub-goal's best section not already in it, after its first
/// [`SUB_GOAL_KEEP`]; then the rest of `found`; `k` sections at most. A single-goal question that
/// uses creative words ([`TECHNIQUES`]) is looked up once more with their techniques.
fn with_sub_goals(mode: crate::coach_meaning::SearchMode, found: Vec<coach_ask::Retrieved>, question: &str, k: usize) -> Vec<coach_ask::Retrieved> {
    let mut goals = if SPLIT_SUB_GOALS { sub_goals(question) } else { Vec::new() };
    if goals.is_empty() {
        let q = chars(question, MAX_QUESTION_CHARS).to_lowercase();
        if with_techniques(&q) == q {
            return found;
        }
        goals.push(q);
    }
    let mut out: Vec<coach_ask::Retrieved> = found.iter().take(SUB_GOAL_KEEP).cloned().collect();
    let mut extra: Vec<coach_ask::Retrieved> = Vec::new();
    for g in &goals {
        let got = coach_ask::retrieve_in(mode, &with_techniques(g), SUB_GOAL_KEEP.saturating_add(1));
        if let Some(r) = got.into_iter().find(|r| !out.iter().chain(&extra).any(|o| std::ptr::eq(o.section, r.section))) {
            extra.push(r);
        }
    }
    for r in extra.into_iter().chain(found.into_iter().skip(SUB_GOAL_KEEP)) {
        if out.len() >= k {
            break;
        }
        if !out.iter().any(|o| std::ptr::eq(o.section, r.section)) {
            out.push(r);
        }
    }
    out
}

/// Whether a grounded answer opens by saying the guide doesn't cover the question.
fn says_dont_know(g: &coach_ask::Grounded) -> bool {
    const SAYS: [&str; 8] = ["don't know", "do not know", "not sure", "don't cover", "do not cover", "doesn't cover", "does not cover", "not covered"];
    g.lines.iter().take(2).any(|l| {
        let t = l.text.to_lowercase().replace('\u{2019}', "'");
        SAYS.iter().any(|w| t.contains(w))
    })
}

/// Answer `question` with `model` on `backend` (blocking: run it off the UI thread), grounded
/// against `checks`; the keyword coach answers when the model can't run, fails, or none of its
/// text passes the check.
pub fn answer(backend: &dyn ExplainBackend, model: &filmcraft_llm::ModelInfo, question: &str, ctx: &AssistContext, checks: &coach_ask::Checks) -> AssistAnswer {
    answer_streaming(backend, model, question, ctx, checks, &mut |_| true)
}

/// [`answer`], calling `on_text` with the model's unchecked text so far as it grows (for a live
/// preview; only the returned answer is grounded). `on_text` returning false stops the model,
/// and the keyword coach answers.
pub fn answer_streaming(
    backend: &dyn ExplainBackend,
    model: &filmcraft_llm::ModelInfo,
    question: &str,
    ctx: &AssistContext,
    checks: &coach_ask::Checks,
    on_text: &mut dyn FnMut(&str) -> bool,
) -> AssistAnswer {
    let (prompt, steps) = match ctx.style {
        AssistStyle::Free => (assistant_prompt(question, ctx, DEFAULT_BUDGET_CHARS), Vec::new()),
        AssistStyle::Pick => {
            let p = pick_prompt(question, ctx, DEFAULT_BUDGET_CHARS);
            (p.prompt, p.steps)
        }
    };
    let pick = ctx.style == AssistStyle::Pick;
    let coach = |sources: Vec<String>, why: AssistFallback| AssistAnswer {
        reference: None,
        reference_seen: None,
        seen: Vec::new(),
        sources,
        grounded: None,
        fallback: Some(why),
        dont_know: false,
        lesson: None,
        coach: Some(coach_ask::ask(question, checks)),
    };
    if prompt.sources.is_empty() {
        return coach(prompt.sources, AssistFallback::NothingFound);
    }
    if let Err((_, why)) = backend.ready(model) {
        return coach(prompt.sources, AssistFallback::NotReady(why));
    }
    let max_tokens = if pick {
        PICK_ANSWER_TOKENS
    } else if prompt.gap.is_some() {
        GAP_ANSWER_TOKENS
    } else {
        MAX_ANSWER_TOKENS
    };
    let opts = filmcraft_llm::GenOptions { max_tokens, deadline: ANSWER_DEADLINE, ..model.recommended_options() };
    let mut stopped = false;
    let mut watch = |t: &str| {
        // a pick reply is keys: the preview shows the steps they name
        let go = if pick { on_text(&picked_text(&answer_picks(t, &steps))) } else { on_text(t) };
        stopped |= !go;
        go
    };
    let text = match backend.generate_streaming(model, &prompt.system, &prompt.user, opts, &mut watch) {
        Ok(_) if stopped => return coach(prompt.sources, AssistFallback::Error("stopped before the answer was finished".into())),
        Ok(t) if pick => {
            let picks = answer_picks(&t, &steps);
            if picks.is_empty() && prompt.gap.is_none() {
                // "NONE": the guide's closest section, and the keyword coach's answer with it
                let closest = prompt.sources.first().cloned().unwrap_or_default();
                format!("The guide sections found don't cover this; the closest is [{closest}].")
            } else {
                picked_text(&picks)
            }
        }
        Ok(t) => t,
        Err(e) => return coach(prompt.sources, AssistFallback::Error(e)),
    };
    // picked steps are the guide's own text, whose ids the guide's tests check against the app
    // (the `coach_ui` walk); a session without the UI's id list would wrongly drop them
    let all = |_: &str| true;
    let guide_checks = coach_ask::Checks { command: &all, namespace: &all, ui: Some(&all) };
    let mut g = coach_ask::ground_with_text(&text, &prompt.sources, &prompt.sent_text, if pick { &guide_checks } else { checks });
    // a missing feature is said first, whatever the model wrote
    if let Some((id, gap)) = &prompt.gap {
        let said = g.lines.first().is_some_and(|l| {
            let t = l.text.to_lowercase();
            t.contains("not yet") || t.contains("doesn't have") || t.contains("does not have") || t.contains("not available")
        });
        if !said {
            // a picked step that is this same statement goes (it read twice, AI3.6 re-read)
            let same = same_start(gap);
            g.lines.retain(|l| same_start(&l.text) != same);
            g.lines.insert(0, coach_ask::GroundedLine { text: format!("{gap} [{id}]"), general: false, ui: Vec::new(), commands: Vec::new() });
            if !g.cited.contains(id) {
                g.cited.insert(0, id.clone());
            }
        }
    }
    if g.lines.is_empty() {
        return coach(prompt.sources, AssistFallback::Ungrounded);
    }
    // "I don't know": keep the model's pointer to the closest section, and give the keyword
    // coach's answer with it
    let dont_know = prompt.gap.is_none() && says_dont_know(&g);
    let coach = dont_know.then(|| coach_ask::ask(question, checks));
    let lesson = if dont_know { None } else { coach_ask::lesson_for(question, if g.cited.is_empty() { &prompt.sources } else { &g.cited }) };
    AssistAnswer {
        reference: None,
        reference_seen: None,
        seen: Vec::new(),
        sources: prompt.sources,
        grounded: Some(g),
        fallback: None,
        dont_know,
        lesson,
        coach,
    }
}

// ---------- jobs and commands (AI3.3d) ----------

/// The assistant's model until a preference picks another (FORK-PLAN §1a.74).
pub const ASSIST_MODEL: &str = "smollm3-3b-q4km";
/// Answers kept for `coach.assistResult` (the oldest go first).
const MAX_KEPT: usize = 32;
/// Questions waiting or running at once.
pub const MAX_PENDING: usize = 4;
/// The longest live preview `coach.assistResult` returns (chars).
pub const MAX_PARTIAL_CHARS: usize = 16_000;

impl AssistAnswer {
    /// The answer as it may leave the session (training inbox): never the reference picture's
    /// numbers or description, and an answer about a reference picture keeps none of its text
    /// (the model may have repeated what it was told about the picture). REF privacy rule.
    pub fn shareable_json(&self) -> Value {
        let mut v = self.to_json();
        if let Some(m) = v.as_object_mut() {
            m.remove("reference");
            if self.reference.is_some() {
                for k in ["lines", "coach", "seen"] {
                    m.remove(k);
                }
                m.insert("privateReference".into(), json!(true));
            }
        }
        v
    }

    pub fn to_json(&self) -> Value {
        let lines: Vec<Value> =
            self.grounded.iter().flat_map(|g| &g.lines).map(|l| json!({"text": l.text, "general": l.general, "ui": l.ui, "commands": l.commands})).collect();
        let g = self.grounded.as_ref();
        json!({
            "source": if self.grounded.is_some() { "model" } else { "coach" },
            "sources": self.sources,
            "lines": lines,
            "cited": g.map(|g| g.cited.clone()).unwrap_or_default(),
            "removedLines": g.map_or(0, |g| g.removed_lines),
            "removedIds": g.map(|g| g.removed_ids.clone()).unwrap_or_default(),
            "inventedCitations": g.map(|g| g.invented_citations.clone()).unwrap_or_default(),
            "fallback": self.fallback,
            "dontKnow": self.dont_know,
            "lesson": self.lesson.as_ref().map(|(id, title)| json!({"id": id, "title": title})),
            "coach": self.coach.as_ref().map(coach_ask::Answer::to_json),
            "seen": self.seen,
            "reference": self.reference.as_ref().map(|r| {
                let mut v = r.to_json();
                v["seen"] = json!(self.reference_seen);
                v
            }),
        })
    }
}

/// Questions being answered in the background, by id.
#[derive(Default)]
pub struct AssistJobs {
    /// `None` while the answer is being written.
    entries: Arc<Mutex<BTreeMap<u64, Option<AssistAnswer>>>>,
    /// The model's unchecked text so far, while an answer is being written.
    partial: Arc<Mutex<BTreeMap<u64, String>>>,
    next: u64,
    /// One model answer at a time.
    gate: Arc<Mutex<()>>,
    /// A backend for tests; `None`: the process's real model backend.
    pub backend: Option<Arc<dyn ExplainBackend>>,
    /// A picture looker for tests; `None`: the installed vision model, if any (AI4.2).
    pub looker: Option<Arc<dyn PictureLooker>>,
    /// The last few looks by (frame content, question): asking again about the same frame
    /// doesn't wait for the vision model again.
    looks: Arc<Mutex<std::collections::VecDeque<(u64, String)>>>,
    /// AI4.2b: the latest look per shot ([`AssistContext::shot`]), newest last.
    shot_looks: Arc<Mutex<std::collections::VecDeque<(u64, String)>>>,
    /// AI4.1c: the last picture fact sheet and its key (clip, frame, the clip's state, the
    /// pipeline), so asking again on the same frame doesn't render it again.
    pub(crate) picture_cache: Mutex<Option<(u64, String)>>,
    /// AI5: each question with its context, rating and correction, for `coach.shareExample`.
    pub records: crate::coach_share::ShareRecords,
}

impl std::fmt::Debug for AssistJobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistJobs").field("next", &self.next).finish_non_exhaustive()
    }
}

/// The registered command ids at the time of the question (the worker can't read the session).
struct Snapshot {
    ids: HashSet<String>,
    ui: Option<Arc<dyn coach_ask::UiIdCheck>>,
}

impl Snapshot {
    fn of(s: &Session) -> Snapshot {
        Snapshot { ids: s.shortcuts.commands().into_iter().map(|c| c.id).collect(), ui: s.coach_ids.clone() }
    }

    fn with<R>(&self, f: impl FnOnce(&coach_ask::Checks) -> R) -> R {
        let command = |id: &str| self.ids.contains(id);
        let namespace = |ns: &str| !ns.is_empty() && self.ids.iter().any(|id| id.strip_prefix(ns).is_some_and(|r| r.starts_with('.')));
        let ui_fn = |id: &str| self.ui.as_ref().is_some_and(|c| c.known_ui_id(id));
        f(&coach_ask::Checks { command: &command, namespace: &namespace, ui: self.ui.is_some().then_some(&ui_fn as &dyn Fn(&str) -> bool) })
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps `seen` as the shot's latest look (one per shot, at most [`MAX_LOOKS`] shots).
fn remember_shot_look(l: &mut std::collections::VecDeque<(u64, String)>, shot: u64, seen: &str) {
    l.retain(|(k, _)| *k != shot);
    while l.len() >= MAX_LOOKS {
        l.pop_front();
    }
    l.push_back((shot, seen.to_string()));
}

/// Whether `question` points at the shot itself ("what would you do with this shot?", "the
/// footage feels wrong") rather than only using "this" for a control: then a look already made
/// of the shot is worth handing on. Narrower than [`asks_about_the_frame`] on purpose.
fn refers_to_the_shot(question: &str) -> bool {
    const SHOT: [&str; 10] = ["shot", "frame", "footage", "picture", "image", "looks", "look", "muddy", "lifeless", "scene"];
    let l = question.to_lowercase();
    l.split(|c: char| !c.is_alphanumeric()).any(|w| SHOT.contains(&w))
}

impl AssistJobs {
    /// AI4.2b: the latest look of `shot`, if the vision model has looked at it.
    pub(crate) fn shot_look(&self, shot: u64) -> Option<String> {
        lock(&self.shot_looks).iter().rev().find(|(k, _)| *k == shot).map(|(_, v)| v.clone())
    }

    /// Start answering; the id to ask [`AssistJobs::result`] with. Errors when too many wait.
    /// The looker questions use: the test one, else the installed vision model.
    pub(crate) fn looker(&self) -> Option<Arc<dyn PictureLooker>> {
        self.looker.clone().or_else(crate::ai_explain::real_looker)
    }

    fn start(
        &mut self,
        snap: Snapshot,
        model: &'static filmcraft_llm::ModelInfo,
        question: String,
        mut ctx: AssistContext,
    ) -> std::result::Result<u64, String> {
        let mut entries = lock(&self.entries);
        if entries.values().filter(|e| e.is_none()).count() >= MAX_PENDING {
            return Err(format!("{MAX_PENDING} questions are already being answered; wait for one"));
        }
        self.next = self.next.saturating_add(1);
        let id = self.next;
        while entries.len() >= MAX_KEPT {
            let Some(oldest) = entries.keys().next().copied() else { break };
            entries.remove(&oldest);
            lock(&self.partial).remove(&oldest);
        }
        let backend = self.backend.clone().or_else(|| crate::ai_explain::real_backend().map(|b| b as Arc<dyn ExplainBackend>));
        let Some(backend) = backend else {
            let a = snap.with(|c| AssistAnswer {
                reference: None,
                reference_seen: None,
                seen: Vec::new(),
                sources: assistant_prompt(&question, &ctx, DEFAULT_BUDGET_CHARS).sources,
                grounded: None,
                fallback: Some(AssistFallback::NotReady("this build can't run a model (built without the `llm` feature)".into())),
                dont_know: false,
                lesson: None,
                coach: Some(coach_ask::ask(&question, c)),
            });
            let mut a = a;
            a.seen = picture_note(&question, ctx.picture_facts.as_deref());
            a.reference = ctx.reference.clone();
            entries.insert(id, Some(a));
            return Ok(id);
        };
        entries.insert(id, None);
        drop(entries);
        let (store, partial, gate) = (self.entries.clone(), self.partial.clone(), self.gate.clone());
        let looker = (ctx.picture.is_some() || ctx.reference_picture.is_some()).then(|| self.looker()).flatten();
        let looks = self.looks.clone();
        let shot_looks = self.shot_looks.clone();
        let spawned = std::thread::Builder::new().name("filmcraft-assist".into()).spawn(move || {
            let _turn = lock(&gate);
            if !matches!(lock(&store).get(&id), Some(None)) {
                return; // forgotten or stopped (`ai.stop`) while it waited
            }
            // AI4.2: the vision model looks at the frame with the question first (its failure
            // only means no "seen" line)
            // REF.4: and at the reference picture, with its own prompt (what numbers can't say)
            if let (Some(pic), Some(looker)) = (ctx.reference_picture.take(), looker.as_ref()) {
                let prompt = crate::coach_reference::reference_look_prompt(&question);
                let key = look_key(&pic, &prompt);
                let cached = lock(&looks).iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
                ctx.reference_seen = cached.or_else(|| {
                    let seen = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| looker.look_with(&pic, &prompt)));
                    let text = Some(chars(seen.ok()?.ok()?.trim(), MAX_SEEN_CHARS)).filter(|t| !t.is_empty())?;
                    let mut l = lock(&looks);
                    if l.len() >= MAX_LOOKS {
                        l.pop_front();
                    }
                    l.push_back((key, text.clone()));
                    Some(text)
                });
            }
            if let (Some(pic), Some(looker)) = (ctx.picture.take(), looker) {
                let key = look_key(&pic, &question);
                let cached = lock(&looks).iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
                ctx.picture_seen = cached.or_else(|| {
                    let seen = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| looker.look(&pic, &question)));
                    let text = Some(chars(seen.ok()?.ok()?.trim(), MAX_SEEN_CHARS)).filter(|t| !t.is_empty())?;
                    let mut l = lock(&looks);
                    if l.len() >= MAX_LOOKS {
                        l.pop_front();
                    }
                    l.push_back((key, text.clone()));
                    Some(text)
                });
                if let (Some(shot), Some(seen)) = (ctx.shot, ctx.picture_seen.as_ref()) {
                    remember_shot_look(&mut lock(&shot_looks), shot, seen);
                }
            }
            // a question forgotten while it waited or ran stops the model
            let mut preview = |t: &str| {
                if !lock(&store).contains_key(&id) {
                    return false;
                }
                lock(&partial).insert(id, chars(t, MAX_PARTIAL_CHARS));
                true
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                snap.with(|c| answer_streaming(backend.as_ref(), model, &question, &ctx, c, &mut preview))
            }));
            lock(&partial).remove(&id);
            let seen = picture_note(&question, ctx.picture_facts.as_deref());
            let mut a = r.unwrap_or_else(|_| AssistAnswer {
                reference: None,
                reference_seen: None,
                seen: Vec::new(),
                sources: Vec::new(),
                grounded: None,
                fallback: Some(AssistFallback::Error("the assistant stopped unexpectedly".into())),
                dont_know: false,
                lesson: None,
                coach: snap.with(|c| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| coach_ask::ask(&question, c))).ok()),
            });
            a.seen = seen;
            a.reference = ctx.reference.clone();
            a.reference_seen = ctx.reference_seen.clone();
            // (a forgotten question stays forgotten, a stopped one stopped)
            if let Some(slot @ None) = lock(&store).get_mut(&id) {
                *slot = Some(a);
            }
        });
        if let Err(e) = spawned {
            lock(&self.entries).remove(&id);
            return Err(format!("could not start the assistant: {e}"));
        }
        Ok(id)
    }

    /// Forget a question: a running answer stops at its next words, a waiting one never starts
    /// the model. False: unknown id.
    pub fn forget(&self, id: u64) -> bool {
        let known = lock(&self.entries).remove(&id).is_some();
        lock(&self.partial).remove(&id);
        known
    }

    /// Stop every question still being answered or waiting (`ai.stop`): each gets a "stopped"
    /// answer now; a running model stops at its next words. Returns how many.
    pub fn stop_all(&self) -> usize {
        let mut entries = lock(&self.entries);
        let mut n = 0;
        for slot in entries.values_mut().filter(|a| a.is_none()) {
            *slot = Some(AssistAnswer {
                reference: None,
                reference_seen: None,
                seen: Vec::new(),
                sources: Vec::new(),
                grounded: None,
                fallback: Some(AssistFallback::Error(crate::ai_explain::STOPPED.into())),
                dont_know: false,
                lesson: None,
                coach: None,
            });
            n += 1;
        }
        drop(entries);
        lock(&self.partial).clear();
        n
    }

    /// Questions still being answered or waiting.
    pub fn pending(&self) -> usize {
        lock(&self.entries).values().filter(|a| a.is_none()).count()
    }

    /// The model's unchecked text so far for a question still being answered.
    pub fn partial(&self, id: u64) -> Option<String> {
        lock(&self.partial).get(&id).cloned()
    }

    /// `None`: unknown id; `Some(None)`: still being written.
    pub fn result(&self, id: u64) -> Option<Option<AssistAnswer>> {
        lock(&self.entries).get(&id).cloned()
    }

    /// [`AssistJobs::result`], waiting up to `cap` for it.
    pub fn wait(&self, id: u64, cap: std::time::Duration) -> Option<Option<AssistAnswer>> {
        let t = web_time::Instant::now();
        loop {
            match self.result(id) {
                // (the web build has no model, so its answers are ready at once: it never sleeps)
                Some(None) if t.elapsed() < cap && !cfg!(target_arch = "wasm32") => std::thread::sleep(std::time::Duration::from_millis(20)),
                other => return other,
            }
        }
    }
}

/// REF: the recipe's own sections go first (the model is told to follow the recipe, so it cites
/// them; without them in the prompt its citations would count as invented). At most
/// [`SECTIONS`] in all.
fn pin_recipe(found: &mut Vec<coach_ask::Retrieved>, ctx: &AssistContext) {
    let Some(r) = &ctx.reference else { return };
    let top = found.iter().map(|f| f.score).filter(|x| x.is_finite()).fold(1.0_f64, f64::max);
    let mut pinned: Vec<coach_ask::Retrieved> = Vec::new();
    for st in r.grade.iter().take(SECTIONS) {
        if !pinned.iter().any(|p| p.header.is_some_and(|h| h.id == st.see))
            && let Some(x) = coach_ask::retrieved_by_id(st.see, top)
        {
            pinned.push(x);
        }
    }
    found.retain(|f| !pinned.iter().any(|p| std::ptr::eq(p.section, f.section)));
    pinned.append(found);
    pinned.truncate(SECTIONS.min(MAX_RETRIEVE));
    *found = pinned;
}

/// REF.4: a still's picture for the vision model, small (about [`LOOK_PIXELS`]), display RGB 0..=1.
fn reference_rgb(s: &Session, still: u64) -> Option<PictureInput> {
    let pic = s.still_picture(still)?;
    let im = image::RgbaImage::from_raw(pic.w, pic.h, pic.rgba)?;
    let area = u64::from(pic.w).saturating_mul(u64::from(pic.h)).max(1);
    let f = ((LOOK_PIXELS as f64) / (area as f64)).sqrt().min(1.0);
    let (w, h) = (((f64::from(pic.w) * f).round() as u32).max(1), ((f64::from(pic.h) * f).round() as u32).max(1));
    let im = if (w, h) == (pic.w, pic.h) { im } else { image::imageops::resize(&im, w, h, image::imageops::FilterType::Triangle) };
    let rgb = im.pixels().flat_map(|p| [p[0], p[1], p[2]].map(|v| f32::from(v) / 255.0)).collect();
    Some(PictureInput { rgb, w: w as usize, h: h as usize })
}

fn opt_text(p: &Value, k: &str) -> Option<String> {
    p.get(k).and_then(Value::as_str).map(|v| chars(v, MAX_CONTEXT_CHARS))
}

fn assist_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let q = p.get("question").and_then(Value::as_str).ok_or_else(|| bad("coach.assist", "need `question` (text)"))?;
    let model_id = p.get("model").and_then(Value::as_str).unwrap_or(ASSIST_MODEL);
    let model = filmcraft_llm::models::find(model_id).ok_or_else(|| bad("coach.assist", format!("unknown model `{}`", chars(model_id, 80))))?;
    // (AI4.1a: `context: "auto"` fills what the caller didn't give from the session: the
    // selection and the current grade node's measured facts; the panel and the Ask box use it)
    let auto = match p.get("context").and_then(Value::as_str) {
        None | Some("none") => false,
        Some("auto") => true,
        Some(_) => return Err(bad("coach.assist", "`context` is `auto` or `none`")),
    };
    let ctx = AssistContext {
        reference: None,
        reference_picture: None,
        reference_seen: None,
        page: opt_text(p, "page"),
        selection: opt_text(p, "selection").or_else(|| auto.then(|| crate::grade_tools::selection_text(s)).flatten().map(|v| chars(&v, MAX_CONTEXT_CHARS))),
        grade_facts_json: opt_text(p, "gradeFacts")
            .or_else(|| auto.then(|| crate::grade_tools::current_grade_facts(s)).flatten().map(|v| chars(&v, MAX_CONTEXT_CHARS))),
        picture_facts: opt_text(p, "pictureFacts")
            .or_else(|| auto.then(|| crate::grade_tools::current_picture_facts(s)).flatten().map(|v| chars(&v, MAX_CONTEXT_CHARS))),
        picture_seen: None,
        picture: None,
        shot: auto.then(|| crate::grade_tools::current_shot_key(s)).flatten(),
        previous_question: opt_text(p, "previousQuestion"),
        previous_answer: opt_text(p, "previousAnswer"),
        style: match p.get("style").and_then(Value::as_str) {
            None => DEFAULT_STYLE,
            Some(v) => AssistStyle::parse(v).ok_or_else(|| bad("coach.assist", "`style` is `free` or `pick`"))?,
        },
    };
    // AI4.2: with a vision model installed and a question about the picture (one the measured
    // facts speak to: exposure, colour, skin, …), the frame goes along for it to look at
    // (rendered small here; the looking happens on the assistant's thread)
    let mut ctx = ctx;
    let about_picture = ctx.picture_facts.as_deref().is_some_and(|f| !picture_note(q, Some(f)).is_empty());
    if auto && about_picture && s.assist.looker().is_some() {
        ctx.picture = crate::grade_tools::program_frame_rgb(s, LOOK_PIXELS).map(|(rgb, w, h)| Arc::new(PictureInput { rgb, w, h }));
    } else if auto && refers_to_the_shot(q) {
        // AI4.2b: a question about the shot that the measured facts don't speak to ("what
        // would you do with this shot?") gets the shot's last look, if there is one; it never
        // waits for a fresh look (provisional design: reuse instead of a generic description)
        ctx.picture_seen = ctx.shot.and_then(|k| s.assist.shot_look(k));
    }
    // REF: a question about a reference picture gets its measured recipe, and the picture goes
    // to the vision model too (the picture itself never leaves the machine, nor the share record)
    if let Some(rid) = crate::coach_reference::reference_for(s, p, q, auto)? {
        ctx.reference = crate::coach_reference::reference_answer(s, rid, &json!({}), "coach.assist").ok().map(Arc::new);
        if ctx.reference.is_some() && s.assist.looker().is_some() {
            ctx.reference_picture = reference_rgb(s, rid).map(Arc::new);
        }
    }
    let snap = Snapshot::of(s);
    let mut record = crate::coach_share::ShareRecord::new(&chars(q, MAX_QUESTION_CHARS), &ctx);
    record.private = Some(Arc::new(crate::coach_share::Private::of(s)));
    let id = s.assist.start(snap, model, chars(q, MAX_QUESTION_CHARS), ctx).map_err(|e| bad("coach.assist", e))?;
    s.assist.records.insert(id, record);
    Ok(json!({"id": id, "model": model.id}))
}

fn result_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad("coach.assistResult", "need `id` (from coach.assist)"))?;
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let r = if wait { s.assist.wait(id, ANSWER_DEADLINE.saturating_add(std::time::Duration::from_secs(30))) } else { s.assist.result(id) };
    Ok(match r {
        None => json!({"state": "unknown"}),
        // `partial`: the model's text so far, not yet checked against the guide
        Some(None) => json!({"state": "pending", "partial": s.assist.partial(id)}),
        Some(Some(a)) => {
            let mut v = a.to_json();
            v["state"] = json!("done");
            v
        }
    })
}

fn forget_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad("coach.assistForget", "need `id` (from coach.assist)"))?;
    Ok(json!({"forgotten": s.assist.forget(id)}))
}

// ---------- AI3.6: scoring the assistant on the advanced set ----------

/// One question of `docs/fork/coach-eval/advanced.toml`.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct EvalCase {
    pub id: String,
    pub kind: String,
    pub q: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub must: Vec<String>,
    #[serde(default)]
    pub must_not: Vec<String>,
}

#[derive(serde::Deserialize)]
struct EvalFile {
    #[serde(default)]
    question: Vec<EvalCase>,
}

/// Most cases read from one eval file (a hostile file can't make the run endless).
pub const MAX_EVAL_CASES: usize = 1000;

/// Parses an advanced-set file (at most [`MAX_EVAL_CASES`] cases).
pub fn parse_eval_set(text: &str) -> std::result::Result<Vec<EvalCase>, String> {
    let mut f: EvalFile = toml::from_str(text).map_err(|e| e.to_string())?;
    f.question.truncate(MAX_EVAL_CASES);
    Ok(f.question)
}

/// The machine-checkable half of a case's score. `must` facts need a reader (person or judge):
/// they come back in `review`, so `auto_pass` is an upper bound on "answered correctly".
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalScore {
    pub id: String,
    /// The model answered (not the keyword coach, not "I don't know").
    pub model_answered: bool,
    /// At least one cited section is one of the case's `sources`.
    pub cites_source: bool,
    /// Lines with unknown controls / commands / labels plus citations of sections not sent:
    /// any of them fails the case (AI-PLAN AI3 accept: "any invented one fails the case").
    pub invented: usize,
    /// `gap` cases, and `creative` cases whose plan has a step FilmCraft can't do yet: the answer
    /// says so (None for other cases).
    pub states_gap: Option<bool>,
    /// `creative` cases: how many of the plan's sections the answer cites (None for other kinds).
    pub plan_cited: Option<usize>,
    pub auto_pass: bool,
    /// Why it failed, in a few words (empty when it passed).
    pub why: Vec<&'static str>,
    /// The case's `must` facts, for the reader.
    pub review: Vec<String>,
}

/// Whether a line says a feature isn't in FilmCraft yet.
fn says_gap(text: &str) -> bool {
    const SAYS: [&str; 8] =
        ["not yet", "not available", "doesn't have", "does not have", "isn't available", "planned", "not in revolution", "not in filmcraft"];
    let t = text.to_lowercase().replace('\u{2019}', "'");
    // "FilmCraft: no outside node yet; …": a clause with both "no" and "yet"
    let no_yet = t.split([';', '.', ',']).any(|c| {
        let w: Vec<&str> = c.split(|ch: char| !ch.is_alphanumeric()).collect();
        w.contains(&"no") && w.contains(&"yet")
    });
    no_yet || SAYS.iter().any(|w| t.contains(w))
}

/// Fewest plan sections a `creative` answer must cite (AI-PLAN AI4 item 5c.3): a creative goal
/// is a plan built from several tools, so one matching section isn't enough (fewer when the plan
/// itself has fewer sections).
pub const CREATIVE_MIN_CITED: usize = 2;

/// [`says_gap`], a plain "can't" / "no built-in", or a clause with a negation and "yet" ("they don't
/// change playback yet"): how creative plans admit a missing step.
fn says_creative_gap(text: &str) -> bool {
    const SAYS: [&str; 5] = ["can't", "cannot", "no built-in", "not possible", "isn't possible"];
    let t = text.to_lowercase().replace('\u{2019}', "'");
    let negated_yet = t.split([';', '.', ',']).any(|c| {
        let w: Vec<&str> = c.split(|ch: char| !ch.is_alphanumeric() && ch != '\'').collect();
        w.contains(&"yet") && w.iter().any(|x| *x == "not" || *x == "no" || x.ends_with("n't"))
    });
    negated_yet || says_gap(&t) || SAYS.iter().any(|w| t.contains(w))
}

/// Scores one answer against its case (AI3.6; `creative` cases as plans, AI4 item 5c.3).
pub fn score_case(case: &EvalCase, a: &AssistAnswer) -> EvalScore {
    let mut s = EvalScore { id: case.id.clone(), review: case.must.clone(), ..EvalScore::default() };
    let Some(g) = a.grounded.as_ref() else {
        s.why.push("the keyword coach answered");
        return s;
    };
    s.model_answered = !a.dont_know;
    if a.dont_know {
        s.why.push("said it doesn't know");
    }
    s.cites_source = g.cited.iter().any(|c| case.sources.contains(c));
    if case.kind == "creative" {
        let cited = case.sources.iter().filter(|src| g.cited.contains(*src)).count();
        s.plan_cited = Some(cited);
        if cited < CREATIVE_MIN_CITED.min(case.sources.len()).max(1) {
            s.why.push("the plan cites fewer than two of its sections");
        }
        // a plan with a step FilmCraft can't do expects the answer to say so, anywhere in it
        if case.must.iter().any(|m| says_creative_gap(m)) {
            let said = g.lines.iter().any(|l| says_creative_gap(&l.text));
            s.states_gap = Some(said);
            if !said {
                s.why.push("didn't say which step Revolution can't do yet");
            }
        }
    } else if !s.cites_source {
        s.why.push("no expected source cited");
    }
    s.invented = g.removed_lines.saturating_add(g.invented_citations.len());
    if s.invented > 0 {
        s.why.push("invented a control, command or citation");
    }
    if case.kind == "gap" {
        let said = g.lines.iter().take(3).any(|l| says_gap(&l.text));
        s.states_gap = Some(said);
        if !said {
            s.why.push("didn't say the feature isn't there yet");
        }
    }
    s.auto_pass = s.why.is_empty();
    s
}

/// Totals over a run of [`score_case`].
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalSummary {
    pub cases: usize,
    pub auto_pass: usize,
    pub model_answered: usize,
    pub cites_source: usize,
    /// Cases with at least one invented control / command / citation.
    pub invented_cases: usize,
    /// Auto-pass rate in percent (0 when there are no cases).
    pub percent: f64,
    /// `creative` cases among `cases`, and how many of them passed: scored apart from lookup
    /// questions (AI4 item 5c.3).
    pub creative_cases: usize,
    pub creative_pass: usize,
}

pub fn summarize(scores: &[EvalScore]) -> EvalSummary {
    let count = |f: &dyn Fn(&EvalScore) -> bool| scores.iter().filter(|s| f(s)).count();
    let cases = scores.len();
    let auto_pass = count(&|s| s.auto_pass);
    EvalSummary {
        cases,
        auto_pass,
        model_answered: count(&|s| s.model_answered),
        cites_source: count(&|s| s.cites_source),
        invented_cases: count(&|s| s.invented > 0),
        percent: if cases == 0 { 0.0 } else { auto_pass as f64 * 100.0 / cases as f64 },
        creative_cases: count(&|s| s.plan_cited.is_some()),
        creative_pass: count(&|s| s.plan_cited.is_some() && s.auto_pass),
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "coach.assist",
            label: "Ask the Assistant",
            menu: &[],
            shortcut: None,
            params: r#"{"question":str,"page":str?,"selection":str?,"gradeFacts":str?,"pictureFacts":str?,"context":str?,"model":str?,"previousQuestion":str?,"previousAnswer":str?,"style":str?}"#,
            enabled: always,
            run: assist_cmd,
            journal: false,
        },
        CommandSpec {
            id: "coach.assistResult",
            label: "Assistant Answer",
            menu: &[],
            shortcut: None,
            params: r#"{"id":int,"wait":bool?}"#,
            enabled: always,
            run: result_cmd,
            journal: false,
        },
        CommandSpec {
            id: "coach.assistForget",
            label: "Stop the Assistant's Answer",
            menu: &[],
            shortcut: None,
            params: r#"{"id":int}"#,
            enabled: always,
            run: forget_cmd,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "coach_assist_tests.rs"]
mod tests;
