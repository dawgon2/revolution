//! "Ask the coach" without a model (fork GRADE-PLAN G2.12, part 2): a question is answered from
//! the fork's user guide (`docs/guide/`, embedded at build time), with checked steps.
//!
//! 1. The guide is split into sections, one per `##` heading of a page (its `###` subsections
//!    included), each with its provenance tags ([Resolve-style], [FilmCraft], [Fork],
//!    [Standard]; a section without its own tag line inherits the page's).
//! 2. Text is tokenised: lower case, colour → color and grey → gray, a stop-word list, a small
//!    English stemmer (plurals, -ing, -ed, -er, -est, a final e) and a synonym table that maps
//!    beginner words to the guide's terms ("sky" → qualifier, "darker" → gamma / gain / lift /
//!    exposure…; expansions count half).
//! 3. Sections are ranked with BM25 (k1 1.2, b 0.5); a word of the section heading counts five
//!    times (the page title once). The confidence is the best score over the most a perfect section could
//!    score for this query (0…1).
//! 4. The answer's steps are the best section's first numbered list, markdown stripped, keeping
//!    the on-screen label text. `{ui:id}` gives a step its control (automation id) and a
//!    `` `command.id` `` its command.
//! 5. **Every id an answer names is checked**: commands against the session's registered command
//!    ids (engine commands plus the ones the frontend registered with the shortcuts registry),
//!    automation ids with the frontend's [`UiIdCheck`] when one is installed
//!    ([`Session::coach_ids`]). A step naming an unknown id is dropped and counted in `removed`.
//! 6. With a low confidence, or no numbered steps left, the answer is the closest lesson instead
//!    ("Try the lesson …") plus the section's summary.
//!
//! `coach.ask {question}` is a query (not journaled). Questions are hostile input: only the first
//! [`MAX_QUESTION_CHARS`] characters are read, words longer than [`MAX_WORD`] are ignored and a
//! query keeps at most [`MAX_QUERY_TERMS`] terms.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};

use crate::Session;
use crate::commands::{CommandSpec, always, bad};

/// Characters of a question that are read (the rest is ignored).
pub const MAX_QUESTION_CHARS: usize = 1000;
/// Longest word that is indexed or searched for, in characters.
pub const MAX_WORD: usize = 40;
/// Most distinct terms in one query (after synonyms).
pub const MAX_QUERY_TERMS: usize = 64;
/// Most steps in one answer.
pub const MAX_STEPS: usize = 20;
/// Longest step text, in characters.
pub const MAX_STEP_TEXT: usize = 600;
/// Longest summary, in characters.
pub const MAX_SUMMARY: usize = 400;
/// Most sources listed with an answer.
pub const MAX_SOURCES: usize = 3;
/// Below this confidence the guide doesn't cover the question (no steps, no sources).
pub const MIN_CONFIDENCE: f64 = 0.15;
/// The share of the question's words (or their synonyms) the best section must contain.
pub const MIN_COVERAGE: f64 = 0.75;
/// A lesson is suggested only when it matches the question at least this well (0…1).
pub const MIN_LESSON: f64 = 0.25;

const K1: f64 = 1.2;
const B: f64 = 0.5;
/// How much a word of a section's heading or page title counts compared with one in the body.
const TITLE_WEIGHT: f64 = 5.0;
/// A "how do I …" question asks for steps: sections that have them count this much more.
const HOW_BOOST: f64 = 1.15;
/// A section whose heading words all appear in the question scores this much more (in part when
/// some do). One-word headings (glossary entries) only count for a "what is …" question.
const HEADING_MATCH: f64 = 0.3;
/// An "X vs Y" question: a section that names both counts this much more.
const COMPARE_BOOST: f64 = 1.5;
/// How much a synonym's expansion counts compared with a word of the question.
const SYNONYM_WEIGHT: f64 = 0.5;

/// The guide pages, relative to `docs/guide/` (a test keeps this list equal to the directory).
pub const GUIDE_PAGES: &[(&str, &str)] = &[
    ("README.md", include_str!("../../../docs/guide/README.md")),
    ("getting-started.md", include_str!("../../../docs/guide/getting-started.md")),
    ("install.md", include_str!("../../../docs/guide/install.md")),
    ("audio.md", include_str!("../../../docs/guide/audio.md")),
    ("cards.md", include_str!("../../../docs/guide/cards.md")),
    ("command-reference.md", include_str!("../../../docs/guide/command-reference.md")),
    ("edit-menu.md", include_str!("../../../docs/guide/edit-menu.md")),
    ("effect-reference.md", include_str!("../../../docs/guide/effect-reference.md")),
    ("compositor.md", include_str!("../../../docs/guide/compositor.md")),
    ("editing.md", include_str!("../../../docs/guide/editing.md")),
    ("essential-sound.md", include_str!("../../../docs/guide/essential-sound.md")),
    ("effects.md", include_str!("../../../docs/guide/effects.md")),
    ("export.md", include_str!("../../../docs/guide/export.md")),
    ("export-queue.md", include_str!("../../../docs/guide/export-queue.md")),
    ("file-menu.md", include_str!("../../../docs/guide/file-menu.md")),
    ("glossary.md", include_str!("../../../docs/guide/glossary.md")),
    ("graphics.md", include_str!("../../../docs/guide/graphics.md")),
    ("hardware.md", include_str!("../../../docs/guide/hardware.md")),
    ("keying.md", include_str!("../../../docs/guide/keying.md")),
    ("linux.md", include_str!("../../../docs/guide/linux.md")),
    ("media.md", include_str!("../../../docs/guide/media.md")),
    ("mixing.md", include_str!("../../../docs/guide/mixing.md")),
    ("packs.md", include_str!("../../../docs/guide/packs.md")),
    ("shader-effects.md", include_str!("../../../docs/guide/shader-effects.md")),
    ("pages.md", include_str!("../../../docs/guide/pages.md")),
    ("projects.md", include_str!("../../../docs/guide/projects.md")),
    ("proxies.md", include_str!("../../../docs/guide/proxies.md")),
    ("render-cache.md", include_str!("../../../docs/guide/render-cache.md")),
    ("network-storage.md", include_str!("../../../docs/guide/network-storage.md")),
    ("shortcuts.md", include_str!("../../../docs/guide/shortcuts.md")),
    ("text.md", include_str!("../../../docs/guide/text.md")),
    ("timeline.md", include_str!("../../../docs/guide/timeline.md")),
    ("tracking.md", include_str!("../../../docs/guide/tracking.md")),
    ("trimming.md", include_str!("../../../docs/guide/trimming.md")),
    ("updates.md", include_str!("../../../docs/guide/updates.md")),
    ("color/README.md", include_str!("../../../docs/guide/color/README.md")),
    ("color/coach.md", include_str!("../../../docs/guide/color/coach.md")),
    ("color/color-management.md", include_str!("../../../docs/guide/color/color-management.md")),
    ("color/curves.md", include_str!("../../../docs/guide/color/curves.md")),
    ("color/finishing.md", include_str!("../../../docs/guide/color/finishing.md")),
    ("color/lumetri.md", include_str!("../../../docs/guide/color/lumetri.md")),
    ("color/matching.md", include_str!("../../../docs/guide/color/matching.md")),
    ("color/nodes.md", include_str!("../../../docs/guide/color/nodes.md")),
    ("color/noise.md", include_str!("../../../docs/guide/color/noise.md")),
    ("color/primaries.md", include_str!("../../../docs/guide/color/primaries.md")),
    ("color/recipes.md", include_str!("../../../docs/guide/color/recipes.md")),
    ("color/secondaries.md", include_str!("../../../docs/guide/color/secondaries.md")),
    ("practice/audio.md", include_str!("../../../docs/guide/practice/audio.md")),
    ("practice/delivery.md", include_str!("../../../docs/guide/practice/delivery.md")),
    ("practice/editing.md", include_str!("../../../docs/guide/practice/editing.md")),
    ("practice/grading.md", include_str!("../../../docs/guide/practice/grading.md")),
    ("practice/looks.md", include_str!("../../../docs/guide/practice/looks.md")),
    ("practice/mixed-cameras.md", include_str!("../../../docs/guide/practice/mixed-cameras.md")),
    ("practice/node-trees.md", include_str!("../../../docs/guide/practice/node-trees.md")),
    ("practice/titles.md", include_str!("../../../docs/guide/practice/titles.md")),
    ("practice/workflow.md", include_str!("../../../docs/guide/practice/workflow.md")),
    ("practice/youtube.md", include_str!("../../../docs/guide/practice/youtube.md")),
];

/// Pages that are about the guide itself (how it is written), not about the app, and pages
/// about the desktop rather than editing or grading (`linux.md`: its words shifted the Coach's
/// ranking of colour questions): not searched.
/// AI3-PREP feature pages after `timeline.md` (AI3-PREP.2d): the 116-question colour eval sits at
/// its floor, and each general page's words pushed it one below; they stay in the guide (and
/// their headers) for AI3's retrieval, which replaces this keyword index. (`compositor.md` is
/// indexed since C1 item 7: the colour set had room after the C1 routes; 91 of its questions still
/// pass with it.)
const NOT_INDEXED: &[&str] = &[
    "README.md",
    "command-reference.md",
    "effect-reference.md",
    "linux.md",
    "timeline.md",
    "effects.md",
    "media.md",
    "shortcuts.md",
    "edit-menu.md",
    "trimming.md",
    "export-queue.md",
    "file-menu.md",
    "practice/grading.md",
    "practice/editing.md",
    "practice/audio.md",
    "practice/delivery.md",
    "practice/node-trees.md",
    "practice/looks.md",
    "practice/youtube.md",
    "practice/titles.md",
    "practice/workflow.md",
    "practice/mixed-cameras.md",
    // fork UPD: about the app's own updates, not editing (its words pushed the pool floors)
    "updates.md",
    // fork PKG: installing each release package, not editing (like updates.md)
    "install.md",
];

/// Sections that are never an answer (page, heading): the glossary's table of Resolve / Premiere
/// names has no steps and outranks the entry it points to (its useful names are synonyms).
const NOT_ANSWERS: &[(&str, &str)] = &[("glossary.md", "At a glance")];

/// The body of a section that isn't written yet (`*To write.*`): not an answer.
const STUB: &str = "To write.";

/// The provenance tags the guide uses.
pub const TAGS: &[&str] = &["[Resolve-style]", "[Revolution]", "[Fork]", "[Standard]"];

/// Checks automation ids against what the frontend has on screen (or has shown this session).
/// The egui UI installs one in [`Session::coach_ids`]; without it, automation ids are passed
/// through unchecked (the answer says `uiChecked: false`).
pub trait UiIdCheck: Send + Sync {
    fn known_ui_id(&self, id: &str) -> bool;
}

/// Words that carry no meaning for the search.
const STOPWORDS: &[&str] = &[
    "a",
    "about",
    "after",
    "all",
    "also",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "been",
    "being",
    "but",
    "by",
    "can",
    "could",
    "did",
    "didn",
    "do",
    "does",
    "doesn",
    "doing",
    "don",
    "e",
    "eg",
    "etc",
    "for",
    "from",
    "get",
    "gets",
    "getting",
    "go",
    "goes",
    "going",
    "gone",
    "got",
    "had",
    "has",
    "have",
    "he",
    "help",
    "her",
    "here",
    "him",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "isn",
    "it",
    "its",
    "just",
    "let",
    "lets",
    "like",
    "make",
    "makes",
    "making",
    "may",
    "me",
    "might",
    "more",
    "most",
    "much",
    "must",
    "my",
    "need",
    "now",
    "of",
    "on",
    "one",
    "or",
    "our",
    "out",
    "please",
    "really",
    "she",
    "should",
    "so",
    "some",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "this",
    "those",
    "to",
    "too",
    "ui",
    "up",
    "use",
    "using",
    "very",
    "want",
    "was",
    "way",
    "we",
    "went",
    "were",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "will",
    "with",
    "without",
    "would",
    "you",
    "your", // contractions written without the apostrophe, and words that only frame a question
    "no",
    "not",
    "vs",
    "whats",
    "thats",
    "hows",
    "wheres",
    "dont",
    "cant",
    "im",
    "ive",
    "isnt",
    "wont",
    "ok",
    "okay",
    "yes",
    // words that only frame a question ("how does the node system work", owner 2026-10-14)
    "system",
    "systems",
    "feature",
    "features",
    "basically",
    "actually",
    "exactly",
    "stuff",
    "things",
    "overall",
    "general",
    // "the node graph", "the curve graph": the noun before it says what is meant
    "graph",
];

/// Beginner words → the guide's terms (each side tokenised like everything else).
const SYNONYMS: &[(&str, &str)] = &[
    ("sky", "qualifier sky"),
    ("audio", "sound"),
    ("laptop", "computer move export import archive"),
    ("stutters", "smooth playback proxies render cache"),
    ("stutter", "smooth playback proxies render cache"),
    ("choppy", "smooth playback proxies render cache"),
    ("blacks", "black point"),
    ("colourist", "interchange otio fcpxml"),
    ("colorist", "interchange otio fcpxml"),
    ("placed", "three point insert overwrite"),
    ("send", "handoff interchange"),
    ("gray", "qualifier matte hue"),
    ("blue", "qualifier"),
    ("bluer", "qualifier"),
    ("skin", "qualifier skin color"),
    ("face", "window face"),
    ("faces", "window face"),
    ("person", "window track face"),
    ("only", "qualifier"),
    ("match", "shot match still"),
    ("cameras", "shot match"),
    ("camera", "shot match"),
    ("noise", "noise reduction"),
    ("noisy", "noise reduction"),
    ("grainy", "noise reduction"),
    ("speckle", "noise reduction"),
    ("darker", "gamma gain lift exposure"),
    ("darken", "gamma gain lift exposure"),
    ("brighter", "gamma gain lift exposure"),
    ("brighten", "gamma gain lift exposure brighten"),
    ("lighter", "gamma gain lift exposure"),
    ("exposure", "gain gamma"),
    ("shadows", "lift shadows"),
    ("highlights", "gain highlight"),
    ("midtones", "gamma"),
    ("mids", "gamma"),
    ("flat", "log2 interpret"),
    ("log", "log2 interpret"),
    ("dull", "saturation contrast"),
    ("washed", "saturation contrast"),
    ("vivid", "saturation"),
    ("colorful", "saturation"),
    ("punchy", "saturation contrast"),
    ("warm", "temperature warm"),
    ("warmer", "temperature warm"),
    ("cold", "temperature"),
    ("cool", "temperature"),
    ("cast", "temperature tint balance"),
    ("neutral", "balance temperature tint"),
    ("white", "balance"),
    ("before", "bypass compare"),
    ("copy", "paste append"),
    ("reuse", "copy paste append"),
    ("track", "track window"),
    ("follow", "track window"),
    ("moving", "track window"),
    ("circle", "window"),
    ("shape", "window"),
    ("mask", "window qualifier matte"),
    ("vignette", "window"),
    ("isolate", "qualifier window"),
    ("select", "qualifier"),
    ("selection", "qualifier"),
    ("clipping", "soft clip highlight"),
    ("clipped", "soft clip highlight"),
    ("blown", "soft clip highlight"),
    ("lut", "lut"),
    ("lesson", "lesson coach"),
    ("tutorial", "lesson coach"),
    ("learn", "lesson coach"),
    ("slow", "hardware decod"),
    ("stutter", "hardware decod playback"),
    ("lag", "hardware playback"),
    ("gpu", "hardware"),
    ("shortcut", "shortcut keymap"),
    ("keyboard", "shortcut keymap"),
    ("explain", "explainer"),
    ("cinematic", "grain look"),
    ("film", "grain"),
    ("off", "disable bypass"),
    ("remove", "delete"),
    ("delete", "remove"),
    ("undo", "undo"),
    // CO1 (docs/fork/coach-eval/DIAGNOSIS.md §4 layer 2), each kept only when the eval set
    // didn't drop. Not `start` (it pulls "Start a lesson"), not `difference / between / vs` →
    // compare (drags "X vs Y" questions to any section that compares), not `album` → gallery
    // (the guide has no albums: that question must decline).
    ("order", "order starter tree"),
    ("first", "order starter tree"),
    ("last", "order starter tree"),
    ("tackle", "order starter tree"),
    ("begin", "order starter tree"),
    ("gradient", "linear graduated"),
    ("pop", "contrast saturation"),
    ("punch", "contrast saturation"),
    ("dark", "gamma gain lift"),
    ("reduce", "reduction"),
    ("reduction", "reduce"),
    ("soften", "soft"),
    ("layout", "workflow page"),
    ("instead", "qualifier window"),
    // Resolve's and Premiere's names (glossary.md › At a glance)
    ("power", "window"),
    ("corrector", "node"),
    ("optimized", "proxy"),
    ("smart", "render cache"),
    ("comparison", "wipe split compare"),
    // AI3.6 adv-audio-voice-fast: the guide writes "clean up" / "clean-up" and "fast"
    ("cleanup", "clean up"),
    ("quickest", "fast quick"),
    // adv-edit-tighten-cut: "the cut is a few frames late" is a trim
    ("late", "trim roll ripple"),
    // AI3.6 lane E: adv-audio-voice-fast ("twenty clips": the guide says "many clips"),
    // adv-qualifier-chatter (a flickering matte edge is "chatter", fixed with softness / blur)
    ("twenty", "many"),
    ("dozens", "many"),
    ("flicker", "chatter softness blur"),
    // typos and cut-off words from real questions
    ("qualifer", "qualifier"),
    ("stil", "still"),
    ("cli", "clip"),
    ("fa", "face"),
];

// ---------- tokenising ----------

fn stopwords() -> &'static std::collections::HashSet<&'static str> {
    static S: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| STOPWORDS.iter().copied().collect())
}

/// British / American spellings to one form.
fn normalise(w: &str) -> String {
    w.replace("colour", "color").replace("grey", "gray").replace("centre", "center").replace("normalis", "normaliz")
}

/// `cutt` → `cut` (after a suffix was removed); not for vowels, l, s or z.
fn undouble(s: &mut String) {
    let mut last = s.bytes().rev();
    if let (Some(a), Some(b)) = (last.next(), last.next())
        && a == b
        && !matches!(a, b'a' | b'e' | b'i' | b'o' | b'u' | b'l' | b's' | b'z')
    {
        s.pop();
    }
}

/// A small English stemmer: plurals, -ing, -ed, -est, -er and a final e. Consistent rather than
/// linguistically right (grade, graded and grading all become "grad").
pub fn stem(w: &str) -> String {
    let mut s = w.to_string();
    if !s.is_ascii() || s.len() <= 3 {
        return s;
    }
    if s.ends_with("sses") {
        s.truncate(s.len() - 2);
    } else if s.ends_with("ies") && s.len() > 4 {
        s.truncate(s.len() - 3);
        s.push('y');
    } else if ["ches", "shes", "xes", "zes"].iter().any(|x| s.ends_with(x)) {
        s.truncate(s.len() - 2);
    } else if s.ends_with('s') && !s.ends_with("ss") && !s.ends_with("us") && !s.ends_with("is") {
        s.pop();
    }
    for suf in ["ing", "ed", "est", "er"] {
        if s.ends_with(suf) && s.len() >= suf.len() + 3 {
            s.truncate(s.len() - suf.len());
            undouble(&mut s);
            break;
        }
    }
    if s.ends_with('e') && s.len() > 3 {
        s.pop();
    }
    s
}

/// The words of a text: lower case, normalised, without stop words, stemmed. Reads at most
/// `max_chars` characters.
pub fn tokenize(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<String>| {
        if !word.is_empty() {
            let w = std::mem::take(word);
            if w.chars().count() <= MAX_WORD && !stopwords().contains(w.as_str()) && (w.chars().count() >= 2) {
                let w = normalise(&w);
                if !stopwords().contains(w.as_str()) {
                    // a stem can be a stop word too ("whats" → "what")
                    let s = stem(&w);
                    if !stopwords().contains(s.as_str()) {
                        out.push(s);
                    }
                }
            }
        }
    };
    for c in text.chars().take(max_chars) {
        if c.is_alphanumeric() {
            if word.chars().count() <= MAX_WORD {
                word.extend(c.to_lowercase());
            }
        } else {
            flush(&mut word, &mut out);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn synonyms() -> &'static HashMap<String, Vec<String>> {
    static S: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    S.get_or_init(|| {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in SYNONYMS {
            if let Some(key) = tokenize(k, 64).into_iter().next() {
                m.entry(key).or_default().extend(tokenize(v, 256));
            }
        }
        m
    })
}

/// Words that make a question a "what is X for" one (C1 item 6, coach-eval DIAGNOSIS §4 route
/// `what-for`).
const PURPOSE_WORDS: &[&str] = &["useful", "good", "use", "purpose", "point", "handy", "helpful"];

/// Whether the question asks what something is for: "what is X good / useful for", "what could I
/// use X for", "when do I use X", "what's the point of X".
fn what_for(question: &str) -> bool {
    let lower = question.chars().take(MAX_QUESTION_CHARS).collect::<String>().to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |w: &str| words.contains(&w);
    let purpose = PURPOSE_WORDS.iter().any(|w| has(w));
    (purpose && has("for")) || (has("when") && has("use")) || (has("point") && has("of"))
}

/// Words that make a question an "X vs Y" one (DIAGNOSIS §4 route `x-vs-y`); they say how to
/// answer, not what about.
const COMPARE_WORDS: &[&str] = &["difference", "differences", "different", "diff", "between", "compared", "versus"];

/// Whether the question compares two things: "difference between X and Y", "X vs Y".
fn x_vs_y(question: &str) -> bool {
    let lower = question.chars().take(MAX_QUESTION_CHARS).collect::<String>().to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |w: &str| words.contains(&w);
    has("vs") || has("versus") || ((has("difference") || has("diff")) && (has("between") || has("and") || has("vs")))
}

/// Words of an order question ("at what point do I…", "where does X go in the chain") that say
/// what kind of answer it wants (DIAGNOSIS §4 route `order`).
const ORDER_WORDS: &[&str] = &["order", "first", "last", "before", "after", "tackle", "then", "point", "go", "should", "chain", "when"];

/// The order words of an order question (stemmed; none for another question). They rank like any
/// word of the question (sections about order use them) but only say what kind of answer is
/// wanted, so the coverage gate counts them only where a section has them.
fn order_words(question: &str) -> Vec<String> {
    if order_question(question) { ORDER_WORDS.iter().flat_map(|w| tokenize(w, 64)).collect() } else { Vec::new() }
}

/// Whether the question asks about order or timing.
fn order_question(question: &str) -> bool {
    let lower = question.chars().take(MAX_QUESTION_CHARS).collect::<String>().to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |w: &str| words.contains(&w);
    has("order")
        || has("tackle")
        || (has("when") && has("should"))
        || (has("what") && has("point"))
        || (has("go") && has("chain"))
        || has("before")
        || has("after")
}

/// The `not-yet` route (DIAGNOSIS §4): in an "is there / do you have / where is / can I" question,
/// the first word that no guide section uses, itself or through a synonym, as typed (at most 40
/// chars). None for other questions or when every word is known.
fn not_yet(question: &str) -> Option<String> {
    let lower = question.chars().take(MAX_QUESTION_CHARS).collect::<String>().to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let starts = |a: &str, b: &str| words.windows(2).any(|p| p.first() == Some(&a) && p.get(1) == Some(&b));
    if !(starts("is", "there") || starts("do", "you") || starts("where", "is") || starts("where", "s") || starts("can", "i")) {
        return None;
    }
    let index = &guide().index;
    let known = |t: &String| index.df.contains_key(t) || synonyms().get(t).is_some_and(|e| e.iter().any(|x| index.df.contains_key(x)));
    words
        .iter()
        .find(|w| {
            let toks = tokenize(w, 64);
            !toks.is_empty() && !toks.iter().all(known)
        })
        .map(|w| w.chars().take(40).collect())
}

/// The query's terms with their weights: the question's words (1) and their synonyms (½).
pub fn query_terms(question: &str) -> Vec<(String, f64)> {
    let mut terms: Vec<(String, f64)> = Vec::new();
    let add = |t: &str, w: f64, terms: &mut Vec<(String, f64)>| {
        if let Some(e) = terms.iter_mut().find(|(x, _)| x == t) {
            e.1 = e.1.max(w);
        } else if terms.len() < MAX_QUERY_TERMS {
            terms.push((t.to_string(), w));
        }
    };
    let mut words = tokenize(question, MAX_QUESTION_CHARS);
    if x_vs_y(question) {
        // "the difference between X and Y": X and Y are the topic
        let kind: Vec<String> = COMPARE_WORDS.iter().flat_map(|w| tokenize(w, 64)).collect();
        words.retain(|w| !kind.contains(w));
    }
    if what_for(question) {
        // "what is X good for": the purpose words are the question type, not its topic
        let purpose: Vec<String> = PURPOSE_WORDS.iter().flat_map(|w| tokenize(w, 64)).collect();
        words.retain(|w| !purpose.contains(w));
        for w in tokenize("why", 64) {
            add(&w, SYNONYM_WEIGHT, &mut terms);
        }
    }
    for w in &words {
        add(w, 1.0, &mut terms);
    }
    for w in &words {
        if let Some(exp) = synonyms().get(w) {
            for e in exp {
                add(e, SYNONYM_WEIGHT, &mut terms);
            }
        }
    }
    // a word the guide never uses whose synonyms it does (a typo, a cut-off word) is replaced by
    // them: otherwise it counts as an unknown word and sinks the confidence
    let index = &guide().index;
    for w in &words {
        if index.df.contains_key(w) {
            continue;
        }
        let Some(exp) = synonyms().get(w).filter(|e| e.iter().any(|x| index.df.contains_key(x))) else { continue };
        terms.retain(|(t, _)| t != w);
        for e in exp {
            if let Some(t) = terms.iter_mut().find(|(t, _)| t == e) {
                t.1 = 1.0;
            }
        }
    }
    terms
}

// ---------- the guide ----------

/// One `##` section of a guide page.
#[derive(Clone, Debug)]
pub struct Section {
    /// The page, relative to `docs/guide/`.
    pub page: &'static str,
    pub page_title: String,
    /// The `##` heading (the page title for the text before the first one).
    pub heading: String,
    /// Provenance tags, as the guide writes them.
    pub provenance: Vec<&'static str>,
    /// The section's markdown, its `###` subsections included.
    pub body: String,
    /// For a page's first `##` section, the page's introduction (the text under its `#` title):
    /// searched with the section, never shown.
    pub intro: String,
    /// The section's compact header line (`> id: … | does: …`, see [`parse_header`]), taken out
    /// of the body (not searched by the keyword index).
    pub header: Option<String>,
}

// ---------- compact section headers (fork AI-PLAN AI3 0d) ----------

/// Longest header line that is read, in characters.
pub const MAX_HEADER_CHARS: usize = 600;
/// Longest `does:` text, in characters.
pub const MAX_DOES_CHARS: usize = 120;

/// A section's compact header: what a retriever (or a small model) needs without the prose.
/// Written as one blockquote line under the section's tag line:
/// `> id: audio.cleanup-voice | does: … | ui: a.b, c.* | cmd: x.y, z.* | resolve: … | premiere: … | when: … | see: other.id`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// `<page id>.<slug>`: unique in the guide; the page id is its path without `.md`, `/` as `.`.
    pub id: String,
    /// What the feature or technique does, in a few words (required).
    pub does: String,
    /// Automation ids (`{ui:…}` of the section) or prefixes ending in `*`.
    pub ui: Vec<String>,
    /// Command ids or prefixes ending in `*`.
    pub cmd: Vec<String>,
    /// The closest name in DaVinci Resolve / Premiere Pro (`—` for none).
    pub resolve: Option<String>,
    pub premiere: Option<String>,
    /// When to reach for it.
    pub when: Option<String>,
    /// Related section ids.
    pub see: Vec<String>,
}

impl Header {
    /// The header's words for search (not its ids).
    pub fn search_text(&self) -> String {
        [Some(self.does.as_str()), self.when.as_deref(), self.resolve.as_deref(), self.premiere.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(". ")
    }
}

fn is_id(v: &str, star: bool) -> bool {
    let body = if star { v.strip_suffix('*').unwrap_or(v) } else { v };
    !v.is_empty() && v.len() <= 120 && body.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) && !body.starts_with('.')
}

/// The page id of a guide page path (`color/nodes.md` → `color.nodes`).
pub fn page_id(page: &str) -> String {
    page.trim_end_matches(".md").replace('/', ".").to_ascii_lowercase()
}

/// Parse a header line. Errors say what is wrong, for the guide check.
pub fn parse_header(line: &str) -> Result<Header, String> {
    if line.chars().count() > MAX_HEADER_CHARS {
        return Err(format!("longer than {MAX_HEADER_CHARS} characters"));
    }
    let body = line.trim().strip_prefix("> ").ok_or("a header is a blockquote line starting with `> id:`")?;
    let mut h = Header::default();
    let mut seen: Vec<&str> = Vec::new();
    for field in body.split(" | ") {
        let (key, value) = field.split_once(':').ok_or_else(|| format!("`{field}`: expected `key: value`"))?;
        let (key, value) = (key.trim(), value.trim());
        if value.is_empty() {
            return Err(format!("`{key}` is empty"));
        }
        if seen.contains(&key) {
            return Err(format!("`{key}` appears twice"));
        }
        seen.push(key);
        let list = |star: bool| -> Result<Vec<String>, String> {
            let v: Vec<String> = value.split(',').map(|x| x.trim().to_string()).collect();
            match v.iter().find(|x| !is_id(x, star)) {
                Some(bad) => Err(format!("`{key}`: `{bad}` is not an id")),
                None => Ok(v),
            }
        };
        match key {
            "id" if is_id(value, false) && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)) => {
                h.id = value.to_string()
            }
            "id" => return Err(format!("id `{value}`: lower case letters, digits, `.`, `-`")),
            "does" if value.chars().count() <= MAX_DOES_CHARS => h.does = value.to_string(),
            "does" => return Err(format!("`does` is longer than {MAX_DOES_CHARS} characters")),
            "ui" => h.ui = list(true)?,
            "cmd" => h.cmd = list(true)?,
            "see" => h.see = list(false)?,
            "resolve" => h.resolve = Some(value.to_string()),
            "premiere" => h.premiere = Some(value.to_string()),
            "when" => h.when = Some(value.to_string()),
            other => return Err(format!("unknown key `{other}` (id, does, ui, cmd, resolve, premiere, when, see)")),
        }
    }
    if seen.first() != Some(&"id") {
        return Err("the first field must be `id`".into());
    }
    if h.does.is_empty() {
        return Err("`does` is required".into());
    }
    Ok(h)
}

fn heading_level(line: &str) -> Option<(usize, &str)> {
    let n = line.bytes().take_while(|b| *b == b'#').count();
    if (1..=6).contains(&n) && line.as_bytes().get(n) == Some(&b' ') { line.get(n + 1..).map(|t| (n, t.trim())) } else { None }
}

fn tags_in(line: &str) -> Vec<&'static str> {
    TAGS.iter().copied().filter(|t| line.contains(t)).collect()
}

/// A line that only holds provenance tags (and maybe a short description after "·").
fn is_tag_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("**[") && !tags_in(t).is_empty()
}

/// Split a page into its sections.
pub fn split_page(page: &'static str, src: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut page_title = String::new();
    let mut page_tags: Vec<&'static str> = Vec::new();
    let mut cur: Option<Section> = None;
    let mut fence = false;
    let mut want_tags = false;
    // the first line after a `##` heading's tag line may be its compact header
    let mut want_header = false;
    // whether `cur` is the page's introduction (the text before its first `##`)
    let mut in_intro = false;
    for line in src.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
        }
        let head = if fence { None } else { heading_level(line) };
        match head {
            Some((1, t)) => {
                page_title = t.to_string();
                want_tags = true;
                cur = Some(Section {
                    page,
                    page_title: page_title.clone(),
                    heading: page_title.clone(),
                    provenance: Vec::new(),
                    body: String::new(),
                    intro: String::new(),
                    header: None,
                });
                in_intro = true;
                continue;
            }
            Some((2, t)) => {
                // the introduction is folded into the first section: its words rank that
                // section, the answer is the section
                let mut intro = String::new();
                match cur.take() {
                    Some(p) if in_intro => intro = p.body,
                    p => out.extend(p),
                }
                in_intro = false;
                cur = Some(Section {
                    page,
                    page_title: page_title.clone(),
                    heading: t.to_string(),
                    provenance: Vec::new(),
                    body: String::new(),
                    intro,
                    header: None,
                });
                want_tags = true;
                want_header = true;
                continue;
            }
            _ => {}
        }
        let Some(sec) = cur.as_mut() else { continue };
        // the compact header: directly under the heading, before or after the tag line
        if want_header && head.is_none() && line.trim_start().starts_with("> id:") {
            want_header = false;
            sec.header = Some(line.trim().to_string());
            continue;
        }
        if want_tags && !line.trim().is_empty() && head.is_none() {
            want_tags = false;
            if is_tag_line(line) {
                let tags = tags_in(line);
                if sec.heading == page_title && out.is_empty() {
                    page_tags = tags.clone();
                }
                sec.provenance = tags;
                continue;
            }
        }
        if !line.trim().is_empty() {
            want_header = false;
        }
        sec.body.push_str(line);
        sec.body.push('\n');
    }
    out.extend(cur);
    for s in &mut out {
        if s.provenance.is_empty() {
            // the first tag line of a subsection, else the page's
            s.provenance = s.body.lines().find(|l| is_tag_line(l)).map(tags_in).unwrap_or_else(|| page_tags.clone());
        }
    }
    // a page's untagged introduction (a status line) and empty sections aren't answers
    out.retain(|s| !s.body.trim().is_empty() && !s.provenance.is_empty());
    // nor are stubs and the sections listed in NOT_ANSWERS
    out.retain(|s| s.body.trim().trim_matches('*').trim() != STUB && !NOT_ANSWERS.contains(&(s.page, s.heading.as_str())));
    out
}

struct Doc {
    tf: HashMap<String, f64>,
    len: f64,
}

struct Index {
    docs: Vec<Doc>,
    df: HashMap<String, usize>,
    avg: f64,
}

impl Index {
    /// Documents of (title, body): a word of the title counts [`TITLE_WEIGHT`] times.
    fn build(texts: impl Iterator<Item = (String, String)>) -> Index {
        let mut docs = Vec::new();
        let mut df: HashMap<String, usize> = HashMap::new();
        for (title, body) in texts {
            let mut tf: HashMap<String, f64> = HashMap::new();
            for t in tokenize(&title, 4096) {
                *tf.entry(t).or_default() += TITLE_WEIGHT;
            }
            for t in tokenize(&body, 1 << 20) {
                *tf.entry(t).or_default() += 1.0;
            }
            for t in tf.keys() {
                *df.entry(t.clone()).or_default() += 1;
            }
            let len = tf.values().sum::<f64>();
            docs.push(Doc { tf, len });
        }
        let avg = if docs.is_empty() { 1.0 } else { (docs.iter().map(|d| d.len).sum::<f64>() / docs.len() as f64).max(1.0) };
        Index { docs, df, avg }
    }

    fn idf(&self, t: &str) -> f64 {
        let n = self.docs.len() as f64;
        let df = self.df.get(t).copied().unwrap_or(0) as f64;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// (doc index, score), best first, only scores above 0.
    fn rank(&self, terms: &[(String, f64)]) -> Vec<(usize, f64)> {
        let mut v: Vec<(usize, f64)> = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let s = terms
                    .iter()
                    .map(|(t, w)| {
                        let f = d.tf.get(t).copied().unwrap_or(0.0);
                        if f <= 0.0 {
                            return 0.0;
                        }
                        w * self.idf(t) * f * (K1 + 1.0) / (f + K1 * (1.0 - B + B * d.len / self.avg))
                    })
                    .sum::<f64>();
                (i, s)
            })
            .filter(|(_, s)| *s > 0.0 && s.is_finite())
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    /// The most a section could score for these terms. A word of the question the index lacks
    /// counts with the highest idf (no section has it), so a question about something the guide
    /// doesn't cover scores low; a synonym it lacks doesn't count.
    fn ceiling(&self, terms: &[(String, f64)]) -> f64 {
        terms.iter().map(|(t, w)| if self.df.contains_key(t) || *w >= 1.0 { w * self.idf(t) * (K1 + 1.0) } else { 0.0 }).sum()
    }

    /// Whether document `i` contains the term.
    fn has(&self, i: usize, t: &str) -> bool {
        self.docs.get(i).is_some_and(|d| d.tf.contains_key(t))
    }
}

struct Guide {
    sections: Vec<Section>,
    /// Whether each section has numbered steps.
    has_steps: Vec<bool>,
    /// Each section's heading words.
    heading_terms: Vec<Vec<String>>,
    index: Index,
    lessons: Index,
}

fn guide() -> &'static Guide {
    static G: OnceLock<Guide> = OnceLock::new();
    G.get_or_init(|| {
        let sections: Vec<Section> = GUIDE_PAGES.iter().filter(|(p, _)| !NOT_INDEXED.contains(p)).flat_map(|(p, src)| split_page(p, src)).collect();
        // (the compact headers aren't searched: the keyword index keeps its tuned weights; AI3's
        // retrieval reads them through `Section::header`)
        let index = Index::build(sections.iter().map(|s| (s.heading.clone(), format!("{}\n{}\n{}", s.page_title, s.intro, s.body))));
        let lessons = Index::build(crate::coach::bundled().iter().map(|l| {
            let steps: Vec<&str> = l.steps.iter().flat_map(|s| [s.text.as_str(), s.why.as_str()]).collect();
            (l.title.clone(), format!("{} {}", l.summary, steps.join(" ")))
        }));
        let has_steps = (0..sections.len()).map(|i| steps_section(&sections, i).is_some()).collect();
        let heading_terms = sections.iter().map(|s| tokenize(&s.heading, 512)).collect();
        Guide { sections, has_steps, heading_terms, index, lessons }
    })
}

/// The indexed sections (for tests and tools).
pub fn sections() -> &'static [Section] {
    &guide().sections
}

/// The sections best matching a question: (section index into [`sections`], score), best first.
pub fn rank(question: &str) -> Vec<(usize, f64)> {
    rank_terms(question, &query_terms(question))
}

/// The section whose steps answer for section `i`: itself when it has a numbered list, else the
/// page's "Steps …" section right after it (a "Why" section and its "Steps").
fn steps_section(sections: &[Section], i: usize) -> Option<&Section> {
    let sec = sections.get(i)?;
    if !numbered_list(&sec.body).is_empty() {
        return Some(sec);
    }
    sections.get(i.saturating_add(1)).filter(|n| n.page == sec.page && n.heading.starts_with("Steps") && !numbered_list(&n.body).is_empty())
}

/// BM25; for a "how …" question a section with numbered steps counts [`HOW_BOOST`] times, and a
/// section whose heading words are in the question up to [`HEADING_MATCH`] more (one-word
/// headings only for a "what …" question).
fn rank_terms(question: &str, terms: &[(String, f64)]) -> Vec<(usize, f64)> {
    let g = guide();
    let mut ranked = g.index.rank(terms);
    let lower = question.chars().take(MAX_QUESTION_CHARS).collect::<String>().to_lowercase();
    let has = |word: &str| lower.split(|c: char| !c.is_alphanumeric()).any(|w| w == word);
    let (how, what) = (has("how"), has("what") || has("explain") || has("mean"));
    // "X vs Y": a section that names every topic word beats one that names only X (conjunctive)
    let both: Vec<&String> =
        if x_vs_y(question) { terms.iter().filter(|(t, w)| *w >= 1.0 && g.index.df.contains_key(t)).map(|(t, _)| t).collect() } else { Vec::new() };
    for (i, s) in &mut ranked {
        if both.len() >= 2 && both.iter().all(|t| g.index.has(*i, t)) {
            *s *= COMPARE_BOOST;
        }
        if how && g.has_steps.get(*i).copied().unwrap_or(false) {
            *s *= HOW_BOOST;
        }
        if let Some(h) = g.heading_terms.get(*i).filter(|h| if what { !h.is_empty() } else { h.len() >= 2 }) {
            let hit = h.iter().filter(|t| terms.iter().any(|(q, w)| *w >= 1.0 && q == *t)).count();
            *s *= 1.0 + HEADING_MATCH * hit as f64 / h.len() as f64;
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
}

// ---------- AI3 retrieval (fork AI-PLAN AI3 item 2) ----------

/// Most sections [`retrieve`] returns.
pub const MAX_RETRIEVE: usize = 12;
/// A header's `does:` words count this many times in the assistant index (like a second title).
const HEADER_WEIGHT: usize = 2;
/// Up to this much more for a section whose heading words are all in the question.
const ASSIST_HEADING_MATCH: f64 = 0.5;
/// This much more for a section whose header `when:` quotes an example question whose words are
/// all in the question (`when: "make it look like film"`). Unquoted `when:` text describes a
/// situation ("interviews and documentaries") and gets no boost.
const ASSIST_WHEN_MATCH: f64 = 0.5;

/// The assistant's index: every guide page (the general and practice pages the keyword coach
/// leaves out included), searching each section's compact header too. Separate from the coach's
/// own index, whose weights are tuned to the colour eval.
struct Assist {
    sections: Vec<Section>,
    headers: Vec<Option<Header>>,
    /// Each section's heading words.
    heading_terms: Vec<Vec<String>>,
    /// Each section's quoted header `when:` example terms (empty without one).
    when_terms: Vec<Vec<String>>,
    index: Index,
}

fn assist() -> &'static Assist {
    static A: OnceLock<Assist> = OnceLock::new();
    A.get_or_init(|| {
        let sections: Vec<Section> =
            GUIDE_PAGES.iter().filter(|(p, _)| *p != "README.md" && !NOT_RETRIEVED.contains(p)).flat_map(|(p, src)| split_page(p, src)).collect();
        let headers: Vec<Option<Header>> = sections.iter().map(|s| s.header.as_deref().and_then(|h| parse_header(h).ok())).collect();
        let index = Index::build(sections.iter().zip(&headers).map(|(s, h)| {
            let does = h.as_ref().map(|h| [h.does.as_str()].repeat(HEADER_WEIGHT).join(". ")).unwrap_or_default();
            let words = h.as_ref().map(Header::search_text).unwrap_or_default();
            (s.heading.clone(), format!("{does}\n{words}\n{}\n{}\n{}", s.page_title, s.intro, s.body))
        }));
        let heading_terms = sections.iter().map(|s| tokenize(&s.heading, 512)).collect();
        let when_terms =
            headers.iter().map(|h| h.as_ref().and_then(|h| h.when.as_deref()).and_then(quoted).map(|w| tokenize(w, 512)).unwrap_or_default()).collect();
        Assist { sections, headers, heading_terms, when_terms, index }
    })
}

/// The text between the first pair of double quotes, if any.
fn quoted(s: &str) -> Option<&str> {
    let (_, rest) = s.split_once('"')?;
    rest.split_once('"').map(|(q, _)| q)
}

/// One section [`retrieve`] found.
#[derive(Clone, Debug)]
pub struct Retrieved {
    pub section: &'static Section,
    /// Its compact header, when it has a valid one (what a model is sent first).
    pub header: Option<&'static Header>,
    pub score: f64,
}

/// Pages written for readers and agents that look things up, not sent to the assistant's model:
/// `effect-reference.md` is a long name list that crowded the sections explaining how to do
/// something out of the top results (AI4.5c, measured: creative coverage@6 354 → 350 / 806).
const NOT_RETRIEVED: &[&str] = &["effect-reference.md"];

/// The `k` (at most [`MAX_RETRIEVE`]) guide sections best matching a question, best first, from
/// every page: what the assistant's model is given. BM25 with the coach's synonyms; a "how …"
/// question favours sections with numbered steps; [`NOT_RETRIEVED`] pages aren't in its index.
pub fn retrieve(question: &str, k: usize) -> Vec<Retrieved> {
    retrieve_in(crate::coach_meaning::default_mode(), question, k)
}

/// [`retrieve`] with a chosen ranking (fork §1a.98): words (BM25 as above), meaning (embedding
/// cosine, [`crate::coach_meaning`]) or both fused by reciprocal rank. Meaning and mixed fall back
/// to words without the model. Sections are ordered by the fused rank; each one's score is the best
/// word score times its relevance share: the larger of its word score's share of the best and its
/// cosine's place between [`crate::coach_meaning::COS_FLOOR`] and the best cosine (0 below the
/// floor). So the pick prompt's relevance filters (a share of the best score) still drop unrelated
/// sections, which a rank-based share would not (rank r of a fused list keeps ~61 / (61 + r)).
pub fn retrieve_in(mode: crate::coach_meaning::SearchMode, question: &str, k: usize) -> Vec<Retrieved> {
    use crate::coach_meaning::{RRF_K, SearchMode};
    let a = assist();
    let k = k.min(MAX_RETRIEVE);
    let words = words_ranked(a, question);
    let ranked = match mode {
        SearchMode::Words => words,
        SearchMode::Meaning | SearchMode::Mixed => {
            // a deep enough meaning list that fusion sees the word list's top sections too
            let depth = k.saturating_mul(8).max(64);
            match crate::coach_meaning::rank(question, depth) {
                None => words,
                Some(meant) => {
                    let best_word = words.first().map_or(1.0, |w| w.1).max(f64::MIN_POSITIVE);
                    let best_cos = meant.first().map_or(1.0, |m| m.1);
                    let floor = crate::coach_meaning::COS_FLOOR;
                    let cos_of: HashMap<usize, f64> = meant.iter().copied().collect();
                    let word_of: HashMap<usize, f64> = words.iter().copied().collect();
                    let share = |i: usize| {
                        let by_cos = cos_of.get(&i).map_or(0.0, |c| if best_cos > floor { ((c - floor) / (best_cos - floor)).clamp(0.0, 1.0) } else { 0.0 });
                        let by_word = if mode == SearchMode::Mixed { word_of.get(&i).map_or(0.0, |w| (w / best_word).clamp(0.0, 1.0)) } else { 0.0 };
                        by_cos.max(by_word)
                    };
                    let mut fused: HashMap<usize, f64> = HashMap::new();
                    let w = if mode == SearchMode::Mixed { crate::coach_meaning::meaning_weight() } else { 1.0 };
                    for (r, (i, _)) in meant.iter().enumerate() {
                        *fused.entry(*i).or_default() += w / (RRF_K + r as f64 + 1.0);
                    }
                    if mode == SearchMode::Mixed {
                        for (r, (i, _)) in words.iter().take(depth).enumerate() {
                            *fused.entry(*i).or_default() += 1.0 / (RRF_K + r as f64 + 1.0);
                        }
                    }
                    let mut v: Vec<(usize, f64)> = fused.into_iter().collect();
                    v.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
                    // Mixed: the word search's first sections stay first
                    let keep: Vec<usize> = if mode == SearchMode::Mixed {
                        let (k, share_min) = (crate::coach_meaning::word_keep(), crate::coach_meaning::word_keep_share());
                        words
                            .iter()
                            .take(k.max(crate::coach_assist::SECTIONS))
                            .enumerate()
                            .filter(|(r, w)| *r < k || w.1 >= share_min * best_word)
                            .map(|(_, w)| w.0)
                            .collect()
                    } else {
                        Vec::new()
                    };
                    keep.iter().copied().chain(v.into_iter().map(|(i, _)| i).filter(|i| !keep.contains(i))).map(|i| (i, best_word * share(i))).collect()
                }
            }
        }
    };
    ranked
        .into_iter()
        .take(k)
        .filter_map(|(i, score)| Some(Retrieved { section: a.sections.get(i)?, header: a.headers.get(i).and_then(Option::as_ref), score }))
        .collect()
}

/// Each assistant section's text for the embedding model, in the index's order: heading, the
/// header's `does:` / `when:`, then the body, cut to `max_chars`.
#[cfg(feature = "llm")]
pub(crate) fn assist_embed_texts(max_chars: usize) -> Vec<String> {
    let a = assist();
    a.sections
        .iter()
        .zip(&a.headers)
        .map(|(s, h)| {
            let does = h.as_ref().map(|h| h.does.as_str()).unwrap_or_default();
            let when = h.as_ref().and_then(|h| h.when.as_deref()).unwrap_or_default();
            let body = plain(&s.body, &|_| true).text;
            format!("{}: {}. {does} {when}\n{body}", s.page_title, s.heading).chars().take(max_chars).collect()
        })
        .collect()
}

/// The word ranking of [`retrieve`]: (section index, score), best first.
fn words_ranked(a: &Assist, question: &str) -> Vec<(usize, f64)> {
    let terms = query_terms(question);
    let how = question.chars().take(MAX_QUESTION_CHARS).collect::<String>().to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| w == "how");
    let mut ranked = a.index.rank(&terms);
    for (i, s) in &mut ranked {
        if how && a.sections.get(*i).is_some_and(|sec| !numbered_list(&sec.body).is_empty()) {
            *s *= HOW_BOOST;
        }
        // a section whose heading words the question names ("Shot Match", "Film grain")
        if let Some(h) = a.heading_terms.get(*i).filter(|h| !h.is_empty()) {
            let hit = h.iter().filter(|t| terms.iter().any(|(q, w)| *w >= 1.0 && q == *t)).count();
            *s *= 1.0 + ASSIST_HEADING_MATCH * hit as f64 / h.len() as f64;
        }
        // a section whose `when:` the question says in full
        if a.when_terms.get(*i).is_some_and(|w| !w.is_empty() && w.iter().all(|t| terms.iter().any(|(q, wt)| *wt >= 1.0 && q == t))) {
            *s *= 1.0 + ASSIST_WHEN_MATCH;
        }
    }
    ranked.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
    ranked
}

// ---------- steps ----------

/// One step of an answer.
#[derive(Clone, Debug, PartialEq)]
pub struct AskStep {
    /// What to do, markdown removed (the on-screen labels stay).
    pub text: String,
    /// The automation ids the step names (`{ui:…}`), in order.
    pub ui: Vec<String>,
    /// The command ids the step names (`` `…` ``), in order.
    pub commands: Vec<String>,
}

/// An id-shaped word with a namespace (`grade.addNode`): ASCII letters, digits and `._-`.
fn id_shaped(s: &str) -> bool {
    s.len() <= crate::coach::MAX_ID
        && s.contains('.')
        && s.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !s.ends_with('.')
        && ![".md", ".toml", ".json", ".rs", ".tsv"].iter().any(|x| s.ends_with(x))
}

/// Plain text made from markdown, and the ids it named.
#[derive(Default)]
struct Plain {
    text: String,
    /// `{ui:…}` ids (taken out of the text).
    ui: Vec<String>,
    /// Backticked id-shaped words kept in the text.
    code: Vec<String>,
    /// Backticked id-shaped words taken out of the text (`keep` refused them).
    dropped: Vec<String>,
}

/// Markdown to plain text: `**`, `*`, backticks and `[text](link)` removed, `{ui:…}` taken out
/// (into `ui`). A backticked id-shaped word stays in the text only when `keep` accepts it.
fn plain(src: &str, keep: &dyn Fn(&str) -> bool) -> Plain {
    let mut p = Plain::default();
    let mut out = String::new();
    let mut rest = src;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("{ui:")
            && let Some(end) = r.find('}')
        {
            let id = r.get(..end).unwrap_or("").trim();
            if !id.is_empty() && p.ui.len() < 16 {
                p.ui.push(id.to_string());
            }
            rest = r.get(end + 1..).unwrap_or("");
            continue;
        }
        if c == '`'
            && let Some(end) = rest.get(1..).and_then(|r| r.find('`'))
        {
            let inner = rest.get(1..end + 1).unwrap_or("");
            if !id_shaped(inner) {
                out.push_str(inner);
            } else if keep(inner) {
                if p.code.len() < 16 {
                    p.code.push(inner.to_string());
                }
                out.push_str(inner);
            } else if p.dropped.len() < 16 {
                p.dropped.push(inner.to_string());
            }
            rest = rest.get(end + 2..).unwrap_or("");
            continue;
        }
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(paren) = rest.get(close + 2..).and_then(|r| r.find(')'))
            && !rest.get(1..close).unwrap_or("").contains(['[', ']'])
        {
            out.push_str(rest.get(1..close).unwrap_or(""));
            rest = rest.get(close + 2 + paren + 1..).unwrap_or("");
            continue;
        }
        if c != '*' {
            out.push(if c.is_control() { ' ' } else { c });
        }
        rest = rest.get(c.len_utf8()..).unwrap_or("");
    }
    p.text = tidy(&out);
    p
}

/// Spacing and punctuation left behind when ids are taken out: `(a, , b)` → `(a, b)`, `( )` → ``.
fn tidy(s: &str) -> String {
    let mut s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    for _ in 0..8 {
        let before = s.len();
        for (a, b) in [
            (" ,", ","),
            (" .", "."),
            (" :", ":"),
            (" ;", ";"),
            (" )", ")"),
            ("( ", "("),
            (",,", ","),
            ("(,", "("),
            (",)", ")"),
            ("()", ""),
            ("  ", " "),
            (", .", "."),
            (", :", ":"),
            (": ,", ":"),
        ] {
            s = s.replace(a, b);
        }
        if s.len() == before {
            break;
        }
    }
    s.trim().trim_end_matches(',').trim().to_string()
}

fn numbered(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    t.get(digits..)?.strip_prefix(". ")
}

/// The first numbered list of a section, one string per item (continuation lines joined). As in
/// CommonMark, a list that interrupts a paragraph must start at 1: a wrapped line that happens to
/// begin "100. …" is prose, not a step.
pub fn numbered_list(body: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut blank = false;
    let mut fence = false;
    // the previous line is prose (no list started yet)
    let mut in_para = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fence = !fence;
            if !items.is_empty() {
                break;
            }
            continue;
        }
        if fence {
            continue;
        }
        if let Some(rest) = numbered(line).filter(|_| !(items.is_empty() && in_para && !line.trim_start().starts_with("1. "))) {
            if items.len() >= MAX_STEPS {
                break;
            }
            items.push(rest.to_string());
            blank = false;
            continue;
        }
        let Some(last) = items.last_mut() else {
            in_para = !line.trim().is_empty() && heading_level(line).is_none();
            continue;
        };
        if line.trim().is_empty() {
            blank = true;
            continue;
        }
        let indented = line.starts_with("  ") || line.starts_with('\t');
        if heading_level(line).is_some() || (blank && !indented) {
            break;
        }
        last.push(' ');
        last.push_str(line.trim());
    }
    items
}

/// A section's first paragraph of prose, as plain text (lists, their continuation lines, tables,
/// code and headings skipped). A backticked id stays only when `keep` accepts it.
pub fn summary(body: &str, keep: &dyn Fn(&str) -> bool) -> String {
    let mut para = String::new();
    let mut in_list = false;
    let mut fence = false;
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        if t.is_empty() {
            in_list = false;
            if !para.is_empty() {
                break;
            }
            continue;
        }
        if numbered(t).is_some() || t.starts_with("- ") || t.starts_with("* ") {
            in_list = true;
        }
        let skip = in_list || heading_level(t).is_some() || is_tag_line(t) || t.starts_with('|') || t.starts_with('>');
        if skip {
            if !para.is_empty() {
                break;
            }
            continue;
        }
        para.push_str(t);
        para.push(' ');
    }
    let para = para.replace("**Why:**", "");
    truncate(&plain(&para, keep).text, MAX_SUMMARY)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// A section's steps, markdown stripped and ids extracted (unchecked: every backticked id
/// counts as a command).
pub fn section_steps(body: &str) -> Vec<AskStep> {
    numbered_list(body)
        .iter()
        .map(|item| {
            let p = plain(item, &|_| true);
            AskStep { text: truncate(&p.text, MAX_STEP_TEXT), ui: p.ui, commands: p.code }
        })
        .filter(|s| !s.text.is_empty())
        .collect()
}

// ---------- the answer ----------

/// What the check needs to know about the session.
pub struct Checks<'a> {
    /// A registered command id (engine or frontend).
    pub command: &'a dyn Fn(&str) -> bool,
    /// The first part of a registered command id (`grade` of `grade.addNode`): an id-shaped
    /// word in another namespace (a preference key, a parameter name) is not a command.
    pub namespace: &'a dyn Fn(&str) -> bool,
    /// A known automation id (None: not checked, no UI).
    pub ui: Option<&'a dyn Fn(&str) -> bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub page: &'static str,
    pub heading: String,
    pub provenance: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub question: String,
    /// A word of an "is there / where is" question that no guide section uses (the `not-yet`
    /// route): the answer says the guide doesn't mention it.
    pub missing: Option<String>,
    /// A "why" or "what is X for" question: the summary (the section's why) is shown ahead of
    /// the steps, not only when there are none (DIAGNOSIS §4).
    pub explain: bool,
    pub steps: Vec<AskStep>,
    pub sources: Vec<Source>,
    /// The closest lesson (id, title).
    pub lesson: Option<(String, String)>,
    /// 0…1: how well the best section covers the question.
    pub confidence: f64,
    /// Steps dropped because they named an unknown command or control.
    pub removed: usize,
    /// Of those, how many named an unknown control / an unknown command (a step can do both).
    pub removed_controls: usize,
    pub removed_commands: usize,
    /// Whether the guide covers the question (confidence and word coverage high enough). When
    /// it doesn't, there are no steps, sources or summary.
    pub covered: bool,
    /// The share of the question's words the best section contains (0…1).
    pub coverage: f64,
    /// The index in `sources` of the section the steps come from.
    pub steps_source: usize,
    /// The unknown ids (at most 16).
    pub removed_ids: Vec<String>,
    /// No steps: the answer is the closest lesson and the summary.
    pub fallback: bool,
    /// The best section's first paragraph.
    pub summary: String,
    /// Whether automation ids were checked (a UI is attached).
    pub ui_checked: bool,
}

impl Answer {
    pub fn to_json(&self) -> Value {
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|s| {
                let mut v = json!({"text": s.text});
                if let Some(u) = s.ui.first() {
                    v["ui"] = json!(u);
                }
                if let Some(c) = s.commands.first() {
                    v["command"] = json!(c);
                }
                if s.ui.len() > 1 {
                    v["uis"] = json!(s.ui);
                }
                if s.commands.len() > 1 {
                    v["commands"] = json!(s.commands);
                }
                v
            })
            .collect();
        let lesson = self.lesson.as_ref().map(|(_, t)| t.as_str());
        let message = match (self.fallback, self.covered, lesson) {
            (false, _, _) => Value::Null,
            (true, true, Some(title)) => json!(format!("Try the lesson \u{201c}{title}\u{201d}.")),
            (true, true, None) => json!("The guide has no step-by-step answer for this yet; see the source below."),
            (true, false, _) if self.missing.is_some() => {
                json!(format!("The guide doesn't mention \u{201c}{}\u{201d}.", self.missing.as_deref().unwrap_or_default()))
            }
            (true, false, Some(title)) => json!(format!("The guide doesn't cover this yet. The closest lesson is \u{201c}{title}\u{201d}.")),
            (true, false, None) if query_terms(&self.question).is_empty() => {
                json!("Ask about a grading task, for example \u{201c}how do I make the sky bluer?\u{201d}")
            }
            (true, false, None) => json!("The guide doesn't cover this yet."),
        };
        json!({
            "question": self.question,
            "steps": steps,
            "sources": self.sources.iter().map(|s| json!({"page": s.page, "heading": s.heading, "provenance": s.provenance})).collect::<Vec<_>>(),
            "lesson": self.lesson.as_ref().map(|(id, title)| json!({"id": id, "title": title})),
            "confidence": (self.confidence * 1000.0).round() / 1000.0,
            "removed": self.removed,
            "removedControls": self.removed_controls,
            "removedCommands": self.removed_commands,
            "removedIds": self.removed_ids,
            "covered": self.covered,
            "coverage": (self.coverage * 1000.0).round() / 1000.0,
            "stepsSource": self.steps_source,
            "fallback": self.fallback,
            "summary": self.summary,
            "message": message,
            "uiChecked": self.ui_checked,
            "missing": self.missing,
            "explain": self.explain,
        })
    }
}

/// The share of the question's own words found (themselves or a synonym) in the sections
/// `docs` of the guide index.
fn coverage(terms: &[(String, f64)], docs: &[usize], optional: &[String]) -> f64 {
    coverage_in(&guide().index, terms, docs, optional)
}

/// The same in any index (the lessons').
/// An `optional` word counts when the sections have it and is ignored when they don't.
fn coverage_in(index: &Index, terms: &[(String, f64)], docs: &[usize], optional: &[String]) -> f64 {
    let found = |t: &str| docs.iter().any(|d| index.has(*d, t));
    let hit = |t: &str| found(t) || synonyms().get(t).is_some_and(|e| e.iter().any(|x| found(x)));
    let own: Vec<&String> = terms.iter().filter(|(t, w)| *w >= 1.0 && (!optional.contains(t) || hit(t))).map(|(t, _)| t).collect();
    if own.is_empty() {
        return 0.0;
    }
    let hit = own.iter().filter(|t| hit(t)).count();
    hit as f64 / own.len() as f64
}

/// The bundled lesson closest to `terms`, as (id, title), when its relative score is at least `min`
/// and it covers enough of the terms.
fn best_lesson(terms: &[(String, f64)], min: f64) -> Option<(String, String)> {
    let g = guide();
    let ceiling = g.lessons.ceiling(terms);
    g.lessons
        .rank(terms)
        .first()
        .filter(|(i, _)| coverage_in(&g.lessons, terms, &[*i], &[]) >= MIN_COVERAGE)
        .map(|(i, score)| (*i, if ceiling > 0.0 { score / ceiling } else { 0.0 }))
        .filter(|(_, c)| *c >= min)
        .and_then(|(i, _)| crate::coach::bundled().get(i))
        .map(|l| (l.id.clone(), l.title.clone()))
}

/// The lesson for an assistant answer ("Show me", AI3.5): ranked on the question, with the words of
/// the section ids the answer cites (`color.nodes.why` → "color nodes why") as a lighter hint that
/// doesn't count towards coverage, as (id, title).
pub fn lesson_for(question: &str, cited: &[String]) -> Option<(String, String)> {
    let mut terms = query_terms(question);
    let words: String = cited.iter().take(MAX_RETRIEVE).flat_map(|id| id.split(['.', '-', '_'])).take(64).collect::<Vec<_>>().join(" ");
    for (t, _) in query_terms(&words) {
        if terms.len() >= MAX_QUERY_TERMS {
            break;
        }
        if !terms.iter().any(|(x, _)| *x == t) {
            terms.push((t, 0.5));
        }
    }
    best_lesson(&terms, MIN_LESSON)
}

/// Answer a question from the guide, checking every id it names.
pub fn ask(question: &str, checks: &Checks) -> Answer {
    let g = guide();
    let terms = query_terms(question);
    let ranked = rank_terms(question, &terms);
    let compare = if x_vs_y(question) { COMPARE_BOOST } else { 1.0 };
    let ceiling = g.index.ceiling(&terms) * HOW_BOOST * (1.0 + HEADING_MATCH) * compare;
    let shown: String = question.chars().take(200).map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut a = Answer {
        question: shown.trim().to_string(),
        steps: Vec::new(),
        sources: Vec::new(),
        lesson: None,
        confidence: 0.0,
        removed: 0,
        removed_controls: 0,
        removed_commands: 0,
        covered: false,
        coverage: 0.0,
        steps_source: 0,
        removed_ids: Vec::new(),
        fallback: true,
        summary: String::new(),
        ui_checked: checks.ui.is_some(),
        missing: not_yet(question),
        explain: what_for(question) || question.split(|c: char| !c.is_alphanumeric()).next().is_some_and(|w| w.eq_ignore_ascii_case("why")),
    };
    // the closest lesson, when it is relevant enough
    let pick = |min: f64| best_lesson(&terms, min);
    // a question the guide doesn't cover gets a lesson only when that lesson is clearly about it
    a.lesson = pick(2.0 * MIN_LESSON);
    let Some(&(best, top)) = ranked.first() else { return a };
    a.confidence = if ceiling > 0.0 { (top / ceiling).clamp(0.0, 1.0) } else { 0.0 };
    let Some(sec) = g.sections.get(best) else { return a };
    let from = steps_section(&g.sections, best);
    let from_index = from.filter(|n| !std::ptr::eq(*n, sec)).map(|_| best.saturating_add(1));
    let docs: Vec<usize> = [Some(best), from_index].into_iter().flatten().collect();
    a.coverage = coverage(&terms, &docs, &order_words(question));
    a.covered = a.confidence >= MIN_CONFIDENCE && a.coverage >= MIN_COVERAGE;
    // "is there a …" about a word the guide never uses: say so instead of answering about the rest
    a.covered &= a.missing.is_none();
    if !a.covered {
        return a;
    }
    a.lesson = pick(MIN_LESSON);
    for (i, score) in ranked.iter().take(MAX_SOURCES) {
        if *score < top * 0.4 {
            break;
        }
        if let Some(s) = g.sections.get(*i) {
            a.sources.push(Source { page: s.page, heading: s.heading.clone(), provenance: s.provenance.clone() });
        }
    }
    if let Some(next) = from.filter(|n| !std::ptr::eq(*n, sec)) {
        let src = Source { page: next.page, heading: next.heading.clone(), provenance: next.provenance.clone() };
        match a.sources.iter().position(|x| *x == src) {
            Some(k) => a.steps_source = k,
            None => {
                a.sources.insert(1.min(a.sources.len()), src);
                a.sources.truncate(MAX_SOURCES);
                a.steps_source = 1.min(a.sources.len().saturating_sub(1));
            }
        }
    }
    // an id-shaped word is kept in the text only when it is a registered command
    let keep = |id: &str| (checks.command)(id);
    a.summary = summary(&sec.body, &keep);
    let items = from.map(|f| numbered_list(&f.body)).unwrap_or_default();
    for item in items {
        let raw = plain(&item, &|_| true);
        // a word in a command namespace that isn't a registered command: the step is wrong
        let bad_cmd: Vec<&String> = raw.code.iter().filter(|c| c.split('.').next().is_some_and(|ns| (checks.namespace)(ns)) && !(checks.command)(c)).collect();
        let bad_ui: Vec<&String> = raw.ui.iter().filter(|u| checks.ui.is_some_and(|f| !f(u))).collect();
        if !bad_cmd.is_empty() || !bad_ui.is_empty() {
            a.removed += 1;
            a.removed_commands += usize::from(!bad_cmd.is_empty());
            a.removed_controls += usize::from(!bad_ui.is_empty());
            for id in bad_cmd.into_iter().chain(bad_ui) {
                if a.removed_ids.len() < 16 && !a.removed_ids.contains(id) {
                    a.removed_ids.push(id.clone());
                }
            }
            continue;
        }
        // other id-shaped words (preference keys, parameter names) can't be checked: left out
        let p = plain(&item, &keep);
        if p.text.is_empty() {
            continue;
        }
        // without a UI the controls can't be checked: none are named (the labels stay in the text)
        let ui = if checks.ui.is_some() { p.ui } else { Vec::new() };
        a.steps.push(AskStep { text: truncate(&p.text, MAX_STEP_TEXT), ui, commands: p.code });
    }
    a.fallback = a.steps.is_empty();
    a
}

// ---------- grounding a model's answer (fork AI-PLAN AI3 item 4) ----------

/// Longest model answer checked, in characters.
pub const MAX_ANSWER_CHARS: usize = 16_000;
/// Most lines of a model answer kept.
pub const MAX_ANSWER_LINES: usize = 80;

/// One line of a checked model answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroundedLine {
    /// Plain text (markdown and `{ui:…}` taken out, citations kept).
    pub text: String,
    /// Marked by the model as advice the guide doesn't state (`General advice:`): never a FilmCraft step.
    pub general: bool,
    /// Controls named (`{ui:…}`), all known.
    pub ui: Vec<String>,
    /// Command ids named, all registered.
    pub commands: Vec<String>,
}

/// A model's answer after [`ground`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grounded {
    pub lines: Vec<GroundedLine>,
    /// Sections cited (`[id]`) that were among the sources sent, in order of first use.
    pub cited: Vec<String>,
    /// Lines left out because they named a control or command that doesn't exist.
    pub removed_lines: usize,
    /// The unknown ids (at most 16).
    pub removed_ids: Vec<String>,
    /// Citations of sections that weren't sent (taken out of the text).
    pub invented_citations: Vec<String>,
}

/// `[id]` citations in a line: (the line without the ones `keep` refuses, every cited id).
fn citations(line: &str, keep: &dyn Fn(&str) -> bool) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut ids = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        let (before, from) = rest.split_at(start);
        out.push_str(before);
        let inner = from.get(1..).unwrap_or("");
        let end = inner.find(']').filter(|e| *e <= 120);
        let id = end.and_then(|e| inner.get(..e)).map(str::trim).unwrap_or("");
        // `[text](link)` is a link, not a citation
        let link = end.and_then(|e| inner.get(e + 1..)).is_some_and(|r| r.starts_with('('));
        if let Some(e) =
            end.filter(|_| !link && !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || "._-#/ ".contains(c)) && id.contains(['.', '#']))
        {
            if ids.len() < 16 {
                ids.push(id.to_string());
            }
            if keep(id) {
                out.push_str(from.get(..e + 2).unwrap_or(""));
            }
            rest = inner.get(e + 1..).unwrap_or("");
        } else {
            out.push('[');
            rest = inner;
        }
    }
    out.push_str(rest);
    (out, ids)
}

/// Check a model's answer: a line that names a control or command that doesn't exist is left
/// out (a line in a command namespace that isn't a registered command is an invented step), a
/// citation of a section that wasn't in `sources` is taken out, and lines starting
/// `General advice:` are marked. Bounded by [`MAX_ANSWER_CHARS`] / [`MAX_ANSWER_LINES`].
pub fn ground(answer: &str, sources: &[String], checks: &Checks) -> Grounded {
    let mut g = Grounded::default();
    let text: String = answer.chars().take(MAX_ANSWER_CHARS).collect();
    let sent = |id: &str| sources.iter().any(|s| s == id);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if g.lines.len() >= MAX_ANSWER_LINES {
            break;
        }
        let raw = plain(line, &|_| true);
        let bad_cmd = raw.code.iter().filter(|c| c.split('.').next().is_some_and(|ns| (checks.namespace)(ns)) && !(checks.command)(c));
        let bad_ui = raw.ui.iter().filter(|u| checks.ui.is_some_and(|f| !f(u)));
        let bad: Vec<&String> = bad_cmd.chain(bad_ui).collect();
        if !bad.is_empty() {
            g.removed_lines += 1;
            for id in bad {
                if g.removed_ids.len() < 16 && !g.removed_ids.contains(id) {
                    g.removed_ids.push(id.clone());
                }
            }
            continue;
        }
        let (kept, ids) = citations(line, &sent);
        for id in ids {
            let list = if sent(&id) { &mut g.cited } else { &mut g.invented_citations };
            if list.len() < 16 && !list.contains(&id) {
                list.push(id);
            }
        }
        let p = plain(&kept, &|id| (checks.command)(id));
        let t = p.text.trim();
        if t.is_empty() {
            continue;
        }
        let general = t.to_ascii_lowercase().starts_with("general advice:");
        let ui = if checks.ui.is_some() { p.ui } else { Vec::new() };
        g.lines.push(GroundedLine { text: truncate(t, MAX_STEP_TEXT.saturating_mul(2)), general, ui, commands: p.code });
    }
    g
}

/// Words that make a line an instruction to operate something.
const ACTION_WORDS: &[&str] =
    &["click", "press", "select", "choose", "open", "tick", "drag", "find", "pick", "go", "use", "insert", "tap", "type", "hold", "enable", "turn"];

/// The labels a line names as things to operate: `**bold**` and `"quoted"` spans in a line that
/// says to click / open / press … something, plus keyboard shortcuts (`Ctrl+N`, `Shift+Alt+K`).
fn named_labels(line: &str) -> Vec<String> {
    let lower = line.to_lowercase();
    let action = lower.split(|c: char| !c.is_alphanumeric()).any(|w| ACTION_WORDS.contains(&w));
    let mut out = Vec::new();
    if action {
        for (open, close) in [("**", "**"), ("\"", "\""), ("\u{201c}", "\u{201d}")] {
            let mut rest = line;
            while let Some(a) = rest.find(open) {
                let after = rest.get(a + open.len()..).unwrap_or("");
                let Some(b) = after.find(close) else { break };
                let label = after.get(..b).unwrap_or("").trim().trim_matches(|c: char| c == '"' || c == '*').trim();
                if !label.is_empty() && label.chars().count() <= 60 && out.len() < 16 {
                    out.push(label.to_string());
                }
                rest = after.get(b + close.len()..).unwrap_or("");
            }
        }
    }
    // shortcuts: a modifier word, `+`, a key
    for w in line.split(|c: char| c.is_whitespace() || "(),;".contains(c)) {
        let parts: Vec<&str> = w.split('+').collect();
        if parts.len() >= 2
            && parts.iter().rev().skip(1).all(|m| ["ctrl", "cmd", "alt", "shift", "option", "super", "meta"].contains(&m.to_lowercase().as_str()))
            && parts.last().is_some_and(|k| !k.is_empty() && k.chars().count() <= 6)
            && out.len() < 16
        {
            out.push(w.trim_matches(|c: char| !c.is_alphanumeric()).to_string());
        }
    }
    out
}

/// [`ground`] plus a check of the controls a line names in plain words: each label from
/// [`named_labels`] must appear (case-insensitively) in `sent_text`, the guide text the model was
/// given; a line naming one that doesn't is left out (an invented button, panel or shortcut).
pub fn ground_with_text(answer: &str, sources: &[String], sent_text: &str, checks: &Checks) -> Grounded {
    let sent = sent_text.to_lowercase();
    let mut kept = String::new();
    let mut extra_removed = Vec::new();
    let mut removed = 0usize;
    for line in answer.chars().take(MAX_ANSWER_CHARS).collect::<String>().lines() {
        let unknown: Vec<String> = named_labels(line).into_iter().filter(|l| !sent.contains(&l.to_lowercase())).collect();
        if unknown.is_empty() {
            kept.push_str(line);
            kept.push('\n');
        } else {
            removed = removed.saturating_add(1);
            for l in unknown {
                if extra_removed.len() < 16 && !extra_removed.contains(&l) {
                    extra_removed.push(l);
                }
            }
        }
    }
    let mut g = ground(&kept, sources, checks);
    g.removed_lines = g.removed_lines.saturating_add(removed);
    for l in extra_removed {
        if g.removed_ids.len() < 16 && !g.removed_ids.contains(&l) {
            g.removed_ids.push(l);
        }
    }
    g
}

// ---------- the command ----------

impl Session {
    /// Answer a question with this session's checks (registered commands; automation ids when a
    /// UI installed [`Session::coach_ids`]).
    pub fn coach_ask(&self, question: &str) -> Answer {
        let shortcuts = &self.shortcuts;
        let command = |id: &str| shortcuts.is_known_command(id);
        let namespace = |ns: &str| shortcuts.is_command_namespace(ns);
        let ui_check: Option<Arc<dyn UiIdCheck>> = self.coach_ids.clone();
        let ui_fn = |id: &str| ui_check.as_ref().is_some_and(|c| c.known_ui_id(id));
        let checks = Checks { command: &command, namespace: &namespace, ui: ui_check.is_some().then_some(&ui_fn as &dyn Fn(&str) -> bool) };
        ask(question, &checks)
    }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "coach.ask",
        label: "Ask the Coach",
        menu: &[],
        shortcut: None,
        params: r#"{"question":str}"#,
        enabled: always,
        run: |s, p| {
            let q = p.get("question").and_then(Value::as_str).ok_or_else(|| bad("coach.ask", "need `question` (text)"))?;
            Ok(s.coach_ask(q).to_json())
        },
        journal: false,
    }]
}
