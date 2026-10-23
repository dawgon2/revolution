//! The fact checker that guards AI-written node explanations (GRADE-PLAN §3 G2.11).
//!
//! [`check_explanation`] takes the [`NodeFacts`] of a node and a candidate explanation (English,
//! British or US spelling) and accepts it only when it is a sentence of a small, strict controlled
//! grammar AND every claim the sentence makes agrees with the facts. The text is hostile: any
//! UTF-8, huge, "1e999", "NaN", "½". When in doubt the check FAILS: the caller then falls back to
//! the template [`NodeFacts::sentence`], which is always safe (false accepts are the serious bug;
//! a false reject only costs quality).
//!
//! # Steps
//!
//! 0. A leading node label ("Node 2 'Skin':") must be this node's exact index and name
//!    ([`check_explanation_labeled`]; [`check_explanation`] allows none).
//! 1. [`check_shape`]: not empty, at most [`MAX_CHARS`] characters and [`MAX_SENTENCES`]
//!    sentences, no markdown, lists or line breaks.
//! 2. [`check_vocabulary`]: every word is a terminal of the grammar ([`VOCABULARY`], derived from
//!    [`GRAMMAR_DOC`]), plain punctuation only.
//! 3. A node that doesn't change the picture (disabled, showing its matte, no visible change) may
//!    only use the `noop_body` rule: the phrasings of [`NOOP_PHRASES`] for its state.
//! 4. Otherwise the text must parse with the grammar below; ANY token sequence that doesn't parse
//!    is rejected ([`FactMismatch::Ungrammatical`] with the word position). Parsing yields explicit
//!    claims: a scope (named ranges, the whole picture, or a selected area), a region (inside /
//!    outside the window or selection, a selected subject), and what is claimed (a change of one
//!    dimension with a sign, a strength and an optional amount; untouched; changed slightly; grain
//!    or noise reduction; the share of the frame). Each clause has exactly one scope; a number
//!    belongs to the clause it is written in; hue words appear only as adjectives
//!    ("warmer", "greener", "more magenta") or in "adds a … tint / cast". Words such as "than",
//!    "also" (outside "; also adds grain"), "then", "so", "except" are not in the grammar.
//! 5. Every claim is verified ([`check_explanation`]):
//!    - named ranges must all be touched (and agree); "untouched" / "the same" / "leaving …
//!      untouched" need untouched ranges; the whole picture ("everything", "the picture", "all
//!      tones", "it", no scope at all) needs EVERY range touched and agreeing and, when the node
//!      has a qualifier / window or covers less than 90 % of the frame, a region lead that says
//!      where ("inside it" after a share-of-the-frame claim, "inside the window", "in the
//!      selection"; contrast claims too); "the whole frame / picture" needs a node without a matte
//!      covering ≥ 90 % of the frame; a
//!      selected area ("the selection", "the skin") needs at least one touched range and every
//!      touched range agreeing;
//!    - a direction needs a fact above the template's own threshold (⅙ stop after rounding, 0.01
//!      chroma shift, 10 % saturation) with the same sign; contrast is highlights minus shadows
//!      exposure (or the contrast control's direction);
//!    - strength works on a level: stops ÷ ⅚, cast ÷ 0.08, saturation % ÷ 40, contrast stops.
//!      Weak words (slightly, a little, a bit, gently, barely, subtly, slight tint) need every
//!      level < 0.5 (< ⅝ stop, < 0.04 cast, < 20 %); strong words (much, a lot, strongly,
//!      dramatically, significantly, heavily, strong tint) need every level ≥ 0.8 (≥ ⅔ stop,
//!      ≥ 0.064 cast, ≥ 32 %); "changed slightly" only when no fact of the range reaches the
//!      template threshold;
//!    - amounts: stops only for brightness, within ±⅙ stop or ±10 % ([`TOL_STOPS`],
//!      [`TOL_STOPS_REL`]) of EVERY range of the scope; percent for saturation (the measured
//!      change of every range), contrast (the control) and the share of the frame, within ±3
//!      points or ±10 % ([`TOL_PCT_POINTS`], [`TOL_PCT_REL`]); an explicit sign must match;
//!      amounts must be informative (≥ 1/12 stop, ≥ 1 %, "<N % of the frame" only for N ≤ 5);
//!    - an ellipsis ("…, the midtones by half a stop") repeats the verb of the claim right before
//!      it, only after "," or "and" (not "but", "while", ";") and never a contrast claim;
//!    - "inside the window" needs a window that isn't inverted, "outside the window" an inverted
//!      one; "the selection" a qualifier that isn't inverted, "outside the selection" an inverted
//!      one; skin / face / sky only a non-inverted qualifier whose hue fits (windows never license
//!      them); "the curve", "the lut", "the window", "the qualifier" as subject need that tool;
//!      grain and noise need those controls.
//! 6. Coverage: the text must mention the node's main changes. Every touched range × dimension
//!    above the template threshold gets its level; EVERY change with at least half the largest
//!    level must be covered by a claim of that dimension whose scope includes the range (a
//!    contrast claim does not stand in for brightness). Smaller changes may be left out
//!    ([`FactMismatch::Omission`] otherwise).
//!
//! # Grammar ([`GRAMMAR_DOC`])
//!
//! ```text
#![doc = include_str!("grammar.ebnf")]
//! ```

use std::fmt;
use std::sync::LazyLock;

use super::{CAST_STEPS, NodeFacts, RANGE_NAMES, RangeFacts, WHOLE_FRAME, cast_level, sat_pct, sixths};

/// The controlled grammar as EBNF with its word lists (for the fine-tuning prompt and training
/// data). Terminals are in double quotes; [`VOCABULARY`] is derived from them.
pub const GRAMMAR_DOC: &str = include_str!("grammar.ebnf");

/// The closed vocabulary: every word an explanation may use (lower case, US spelling; British
/// spellings are normalised first): the quoted terminals of [`GRAMMAR_DOC`], sorted.
pub static VOCABULARY: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut v: Vec<&'static str> = GRAMMAR_DOC
        .split('"')
        .skip(1)
        .step_by(2)
        .flat_map(str::split_whitespace)
        .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
});

/// The ways a node that does nothing (disabled, showing its matte, no visible change) may say so.
/// An explanation is split into segments at punctuation; the filler words of the `noop_body` rule
/// are dropped; each remaining segment must equal one of these phrases (disabled nodes: the
/// disabled and no-change phrases; matte view: the matte phrases and, if also disabled, the
/// disabled phrases; otherwise: the no-change phrases).
pub const NOOP_PHRASES: &[&[&str]] = &[
    &["disabled"],
    &["bypassed"],
    &["inactive"],
    &["switched", "off"],
    &["turned", "off"],
    &["off"],
    &["no", "visible", "change"],
    &["no", "change"],
    &["nothing", "changes"],
    &["unchanged"],
    &["untouched"],
    &["showing", "qualifier", "selection"],
    &["showing", "qualifier", "matte"],
    &["showing", "matte"],
    &["showing", "selection"],
    &["highlight", "matte", "view"],
    &["matte", "view"],
    &["not", "grade"],
    &["not", "grading"],
    &["rather", "than", "grading"],
    &["rather", "than", "grade"],
];

const DISABLED_PHRASES: std::ops::Range<usize> = 0..6;
const NOCHANGE_PHRASES: std::ops::Range<usize> = 6..11;
const MATTE_PHRASES: std::ops::Range<usize> = 11..NOOP_PHRASES.len();
const NOOP_DROP: [&str; 15] = ["the", "a", "an", "it", "its", "this", "is", "are", "node", "so", "and", "picture", "image", "frame", "in"];
const ALLOWED_PUNCT: [char; 14] = ['.', ',', ';', ':', '!', '?', '(', ')', '/', '-', '+', '<', '>', '\u{2212}'];

/// The longest explanation (characters).
pub const MAX_CHARS: usize = 300;
/// The most sentences.
pub const MAX_SENTENCES: usize = 2;
/// Stated stops may differ from the facts by this much (⅙ stop plus float slack).
pub const TOL_STOPS: f64 = 1.0 / 6.0 + 0.005;
/// … or this fraction of the fact when that is larger (for several stops).
pub const TOL_STOPS_REL: f64 = 0.10;
/// Percentages: at least this many points of slack …
pub const TOL_PCT_POINTS: f64 = 3.0;
/// … or this fraction of the fact, whichever is larger.
pub const TOL_PCT_REL: f64 = 0.10;

/// Why an explanation was rejected, in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactMismatch {
    Empty,
    TooLong(usize),
    TooManySentences(usize),
    /// Markdown, lists, line breaks or other formatting.
    Formatting(String),
    /// Not plain English text (other scripts).
    NotEnglish(String),
    /// A number that can't be read (huge, "1e999", non-ASCII digits).
    Numeral(String),
    /// A number the facts don't support.
    Number(String),
    /// A direction claim that disagrees with the facts.
    Direction(String),
    /// A claim about a range that is untouched (or "untouched" about one that isn't).
    Range(String),
    /// Describes a no-op as changing something, or a change as no-op.
    NoChange(String),
    /// Something the facts don't contain.
    Unsupported(String),
    /// "slightly" for a strong change or "much" for a weak one.
    Intensity(String),
    /// A word outside the closed [`VOCABULARY`].
    UnknownWord(String),
    /// A text of a changing node with no claim.
    NoClaim(String),
    /// The words don't form a sentence of the grammar: the word position (0-based, punctuation
    /// counts) where parsing got stuck and the word there.
    Ungrammatical(usize, String),
    /// Inside / outside the window or selection, or a selected subject, that the node's
    /// qualifier and windows don't match.
    Selection(String),
    /// The node's dominant change isn't mentioned.
    Omission(String),
    /// A node label ("Node 7 'Skin':") that isn't this node's.
    Label(String),
}

impl fmt::Display for FactMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the explanation is empty"),
            Self::TooLong(n) => write!(f, "the explanation has {n} characters (the limit is {MAX_CHARS})"),
            Self::TooManySentences(n) => write!(f, "the explanation has {n} sentences (the limit is {MAX_SENTENCES})"),
            Self::Formatting(s) => write!(f, "formatting is not allowed: {s}"),
            Self::NotEnglish(s) => write!(f, "only plain English letters are allowed: {s}"),
            Self::Numeral(s) => write!(f, "unreadable number: {s}"),
            Self::Number(s) => write!(f, "number not supported by the facts: {s}"),
            Self::Direction(s) => write!(f, "direction disagrees with the facts: {s}"),
            Self::Range(s) => write!(f, "tonal range disagrees with the facts: {s}"),
            Self::NoChange(s) => write!(f, "change and no-change disagree with the facts: {s}"),
            Self::Unsupported(s) => write!(f, "not in the facts: {s}"),
            Self::Intensity(s) => write!(f, "strength disagrees with the facts: {s}"),
            Self::UnknownWord(w) => write!(f, "\"{w}\" is not in the vocabulary"),
            Self::NoClaim(s) => write!(f, "nothing in the text can be verified: {s}"),
            Self::Ungrammatical(at, w) => write!(f, "not a sentence of the explanation grammar (at word {at}, \"{w}\")"),
            Self::Selection(s) => write!(f, "selection disagrees with the facts: {s}"),
            Self::Omission(s) => write!(f, "a main change is not mentioned: {s}"),
            Self::Label(s) => write!(f, "wrong node label: {s}"),
        }
    }
}

impl std::error::Error for FactMismatch {}

/// Check that `text` explains `facts` truthfully (see the module docs for every rule). Ok means it
/// may be shown; Err gives the first reason it may not (then use [`NodeFacts::sentence`]). The
/// text may not carry a node label ("Node 1: …"); see [`check_explanation_labeled`].
pub fn check_explanation(facts: &NodeFacts, text: &str) -> Result<(), FactMismatch> {
    check_explanation_labeled(facts, None, None, text)
}

/// [`check_explanation`] for a text that may start with the node's label: "Node N:" only with
/// `index == Some(N)`, "Node N 'Name':" only with the exact `label == Some("Name")` as well. Any
/// other label or quote is rejected.
pub fn check_explanation_labeled(facts: &NodeFacts, label: Option<&str>, index: Option<usize>, text: &str) -> Result<(), FactMismatch> {
    check_shape(text)?;
    let text = match split_label(text) {
        None => text,
        Some((n, name, rest)) => {
            if index.is_none_or(|i| i as u64 != n) {
                return Err(FactMismatch::Label(format!("the text is labelled node {n}, which is not this node")));
            }
            if let Some(name) = name
                && label != Some(name)
            {
                return Err(FactMismatch::Label(format!("the text names the node '{}', which is not its name", name.chars().take(40).collect::<String>())));
            }
            rest
        }
    };
    if text.trim().is_empty() {
        return Err(FactMismatch::Empty);
    }
    let toks = tokenize(&prepare(text))?;
    vocabulary_in(&toks)?;
    let active = facts.enabled && !facts.showing_matte && facts.visible();
    if !active {
        return check_noop(facts, &toks);
    }
    let parsed = match parse(&toks) {
        Ok(p) => p,
        Err(e) => {
            let segs = noop_segments(&toks);
            if !segs.is_empty() && segs.iter().all(|s| NOOP_PHRASES.iter().any(|p| phrase_eq(p, s))) {
                return Err(FactMismatch::NoChange("says the node does nothing, but it changes the picture".into()));
            }
            return Err(e);
        }
    };
    Fx { f: facts }.verify(&parsed)
}

/// Steps 1, 2 and 4 without facts: whether `text` is a sentence of the grammar (a no-op phrasing
/// counts too). For training data and prompt tests.
pub fn check_grammar(text: &str) -> Result<(), FactMismatch> {
    check_shape(text)?;
    let text = split_label(text).map_or(text, |(_, _, rest)| rest);
    let toks = tokenize(&prepare(text))?;
    vocabulary_in(&toks)?;
    match parse(&toks) {
        Ok(_) => Ok(()),
        Err(e) => {
            let segs = noop_segments(&toks);
            if !segs.is_empty() && segs.iter().all(|s| NOOP_PHRASES.iter().any(|p| phrase_eq(p, s))) { Ok(()) } else { Err(e) }
        }
    }
}

/// Steps 1 and 2 alone: only words of [`VOCABULARY`] and plain punctuation.
pub fn check_vocabulary(text: &str) -> Result<(), FactMismatch> {
    check_shape(text)?;
    let text = split_label(text).map_or(text, |(_, _, rest)| rest);
    vocabulary_in(&tokenize(&prepare(text))?)
}

fn vocabulary_in(toks: &[Tok]) -> Result<(), FactMismatch> {
    for t in toks {
        match t {
            Tok::Word(w) if VOCABULARY.binary_search(&w.as_str()).is_err() => return Err(FactMismatch::UnknownWord(w.clone())),
            Tok::P(c) if !ALLOWED_PUNCT.contains(c) => return Err(FactMismatch::Formatting(format!("the symbol '{c}'"))),
            _ => {}
        }
    }
    Ok(())
}

/// Step 1 alone (length, sentences, formatting).
pub fn check_shape(text: &str) -> Result<(), FactMismatch> {
    if text.trim().is_empty() {
        return Err(FactMismatch::Empty);
    }
    let n = text.chars().take(MAX_CHARS + 1).count();
    if n > MAX_CHARS {
        return Err(FactMismatch::TooLong(text.chars().count()));
    }
    if text.chars().any(|c| c.is_control()) {
        return Err(FactMismatch::Formatting("line breaks or control characters".into()));
    }
    if let Some(c) = text.chars().find(|c| matches!(c, '*' | '`' | '#' | '_' | '[' | ']' | '|' | '~' | '\\' | '{' | '}')) {
        return Err(FactMismatch::Formatting(format!("markdown character '{c}'")));
    }
    let t = text.trim_start();
    let bullet = ["- ", "• ", "+ ", "> "].iter().any(|b| t.starts_with(b));
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    let numbered = digits > 0 && t.chars().nth(digits).is_some_and(|c| c == '.' || c == ')') && t.chars().nth(digits + 1) == Some(' ');
    if bullet || numbered {
        return Err(FactMismatch::Formatting("a list".into()));
    }
    let cs: Vec<char> = text.chars().collect();
    let (mut sentences, mut open) = (0usize, false);
    for (i, c) in cs.iter().enumerate() {
        if matches!(c, '.' | '!' | '?') && cs.get(i + 1).is_none_or(|n| n.is_whitespace()) {
            if open {
                sentences += 1;
            }
            open = false;
        } else if !c.is_whitespace() && !matches!(c, '.' | '!' | '?') {
            open = true;
        }
    }
    sentences += usize::from(open);
    if sentences > MAX_SENTENCES {
        return Err(FactMismatch::TooManySentences(sentences));
    }
    Ok(())
}

// ---- tokens ---------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// A number with its explicit sign (−1, 0, +1).
    Num {
        v: f64,
        sign: i8,
    },
    Word(String),
    Pct,
    Deg,
    P(char),
}

/// Lower case, US spelling, "mid-tones" → "midtones".
fn prepare(text: &str) -> String {
    let mut s = text.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'").replace(['\u{201c}', '\u{201d}'], "\"");
    for (a, b) in [
        ("mid-tones", "midtones"),
        ("mid tones", "midtones"),
        ("mid-tone", "midtone"),
        ("mid tone", "midtone"),
        ("mid-range", "midrange"),
        ("colour", "color"),
        ("grey", "gray"),
        ("centre", "center"),
        ("whilst", "while"),
    ] {
        s = s.replace(a, b);
    }
    s
}

/// A leading label "Node N:" or "Node N 'Name':" (straight or curly quotes): N, the name and the
/// text after the colon. None when the text doesn't start with a well-formed label (a malformed
/// one then fails as quotes or words outside the grammar).
fn split_label(text: &str) -> Option<(u64, Option<&str>, &str)> {
    let t = text.trim_start();
    if !t.get(..5)?.eq_ignore_ascii_case("node ") {
        return None;
    }
    let rest = t.get(5..)?;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let n: u64 = rest.get(..digits)?.parse().ok()?;
    let rest = rest.get(digits..)?;
    let (name, rest) = match rest.strip_prefix(' ').and_then(|r| r.chars().next().map(|q| (q, r))) {
        Some((q @ ('\'' | '"' | '\u{2018}' | '\u{201c}'), r)) => {
            let close = match q {
                '\u{2018}' => '\u{2019}',
                '\u{201c}' => '\u{201d}',
                c => c,
            };
            let body = r.get(q.len_utf8()..)?;
            let end = body.find(close)?;
            (Some(body.get(..end)?), body.get(end + close.len_utf8()..)?)
        }
        _ => (None, rest),
    };
    Some((n, name, rest.strip_prefix(':')?))
}

fn vulgar(c: char) -> Option<f64> {
    Some(match c {
        '¼' => 0.25,
        '½' => 0.5,
        '¾' => 0.75,
        '⅓' => 1.0 / 3.0,
        '⅔' => 2.0 / 3.0,
        '⅕' => 0.2,
        '⅖' => 0.4,
        '⅗' => 0.6,
        '⅘' => 0.8,
        '⅙' => 1.0 / 6.0,
        '⅚' => 5.0 / 6.0,
        '⅛' => 0.125,
        '⅜' => 0.375,
        '⅝' => 0.625,
        '⅞' => 0.875,
        '⅐' => 1.0 / 7.0,
        '⅑' => 1.0 / 9.0,
        '⅒' => 0.1,
        _ => return None,
    })
}

fn tokenize(s: &str) -> Result<Vec<Tok>, FactMismatch> {
    let cs: Vec<char> = s.chars().collect();
    let digit = |i: usize| cs.get(i).is_some_and(char::is_ascii_digit);
    let mut out = Vec::new();
    let (mut i, mut sign) = (0usize, 0i8);
    while let Some(&c) = cs.get(i) {
        if matches!(c, '+' | '-' | '\u{2212}') {
            let prev = i.checked_sub(1).and_then(|j| cs.get(j)).copied();
            let next_numeric = cs.get(i + 1).is_some_and(|n| n.is_ascii_digit() || vulgar(*n).is_some());
            let prev_alnum = prev.is_some_and(|p| p.is_alphanumeric() || vulgar(p).is_some() || p == '%');
            if next_numeric && !prev_alnum {
                sign = if c == '+' { 1 } else { -1 };
            } else {
                out.push(Tok::P(c));
            }
            i += 1;
        } else if c.is_ascii_digit() || vulgar(c).is_some() {
            let mut v = 0.0;
            if c.is_ascii_digit() {
                let start = i;
                while digit(i) {
                    i += 1;
                }
                if cs.get(i) == Some(&'.') && digit(i + 1) {
                    i += 1;
                    while digit(i) {
                        i += 1;
                    }
                }
                if matches!(cs.get(i), Some('e')) {
                    let mut j = i + 1;
                    if matches!(cs.get(j), Some('+' | '-')) {
                        j += 1;
                    }
                    if digit(j) {
                        while digit(j) {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let lex: String = cs.get(start..i).unwrap_or_default().iter().collect();
                v = lex.parse::<f64>().map_err(|_| FactMismatch::Numeral(lex.chars().take(20).collect()))?;
                if cs.get(i) == Some(&'/') && digit(i + 1) {
                    let mut j = i + 1;
                    while digit(j) {
                        j += 1;
                    }
                    let den: String = cs.get(i + 1..j).unwrap_or_default().iter().collect();
                    let den = den.parse::<f64>().unwrap_or(0.0);
                    if den == 0.0 {
                        return Err(FactMismatch::Numeral("fraction with a zero denominator".into()));
                    }
                    v /= den;
                    i = j;
                } else if cs.get(i) == Some(&' ') && cs.get(i + 1).is_some_and(|n| vulgar(*n).is_some()) {
                    i += 1;
                }
            }
            if let Some(f) = cs.get(i).and_then(|d| vulgar(*d)) {
                v += f;
                i += 1;
            }
            if !v.is_finite() || v.abs() > 1e9 {
                return Err(FactMismatch::Numeral("number out of range".into()));
            }
            out.push(Tok::Num { v, sign });
            sign = 0;
        } else if c.is_numeric() {
            return Err(FactMismatch::Numeral(format!("unsupported numeral '{c}'")));
        } else if c.is_alphabetic() {
            let start = i;
            while cs.get(i).is_some_and(|d| d.is_alphabetic() || *d == '\'') {
                i += 1;
            }
            let w: String = cs.get(start..i).unwrap_or_default().iter().collect();
            if !w.chars().all(|d| d.is_ascii_alphabetic() || d == '\'') {
                return Err(FactMismatch::NotEnglish(w.chars().take(20).collect()));
            }
            if matches!(w.as_str(), "nan" | "inf" | "infinity" | "infinite" | "undefined" | "null") {
                return Err(FactMismatch::Numeral(w));
            }
            out.push(Tok::Word(w));
        } else {
            match c {
                '%' => out.push(Tok::Pct),
                '°' => out.push(Tok::Deg),
                c if c.is_whitespace() => {}
                c => out.push(Tok::P(c)),
            }
            i += 1;
        }
    }
    Ok(out)
}

// ---- claims ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dim {
    Brightness,
    Warmth,
    Tint,
    Saturation,
    Contrast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strength {
    Weak,
    Normal,
    Strong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Stops,
    Percent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Amount {
    v: f64,
    /// The explicit sign (−1, 0, +1).
    sign: i8,
    unit: Unit,
    /// "<1 %".
    below: bool,
}

impl Amount {
    fn stops(v: f64) -> Self {
        Self { v, sign: 0, unit: Unit::Stops, below: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subject {
    /// Skin, faces.
    Skin,
    Sky,
}

/// Where in the frame a claim speaks about (`Some(true)`: outside).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Region {
    window: Option<bool>,
    selection: Option<bool>,
    subject: Option<Subject>,
    /// Restricted to where the node acts (any region, also "inside it"): needed for a
    /// whole-picture claim about a node with a matte or partial coverage.
    area: bool,
}

impl Region {
    const NONE: Self = Self { window: None, selection: None, subject: None, area: false };
    const AREA: Self = Self { window: None, selection: None, subject: None, area: true };

    /// Both regions at once; None when they contradict ("inside" and "outside" the window).
    fn merge(self, o: Self) -> Option<Self> {
        fn m<T: PartialEq + Copy>(a: Option<T>, b: Option<T>) -> Option<Option<T>> {
            match (a, b) {
                (Some(x), Some(y)) if x != y => None,
                (a, b) => Some(a.or(b)),
            }
        }
        Some(Self {
            window: m(self.window, o.window)?,
            selection: m(self.selection, o.selection)?,
            subject: m(self.subject, o.subject)?,
            area: self.area || o.area,
        })
    }
}

/// How a clause is joined to the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Join {
    Start,
    /// "," "and" ", and".
    List,
    /// "while" "but".
    Contrast,
    Semicolon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Span {
    /// Named ranges (shadows, midtones, highlights).
    Ranges([bool; 3]),
    /// The whole picture: every range ("everything", "the picture", no scope at all);
    /// `whole_frame` for "the whole / entire frame".
    Tonal { whole_frame: bool },
    /// A selected area ("the selection", "the skin"): the touched ranges.
    Area,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scope {
    span: Span,
    region: Region,
}

const IMPLICIT: Scope = Scope { span: Span::Tonal { whole_frame: false }, region: Region::NONE };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Texture {
    Grain,
    NoiseReduction,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum What {
    Change {
        dim: Dim,
        sign: i8,
        strength: Strength,
        amount: Option<Amount>,
    },
    Untouched,
    ChangedSlightly,
    Texture(Texture),
    /// The share of the frame (None: the whole frame).
    Coverage(Option<Amount>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Claim {
    scope: Scope,
    region: Region,
    what: What,
    /// Token index, for messages.
    at: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Curve,
    Lut,
    Window,
    Qualifier,
}

#[derive(Debug, Default)]
struct Parsed {
    claims: Vec<Claim>,
    tools: Vec<Tool>,
}

// ---- word classes (the grammar's terminals; see grammar.ebnf) --------------------------------

fn dim_verb(w: &str) -> Option<(Dim, i8)> {
    use Dim::*;
    Some(match w {
        "brightens" | "brighten" | "brightening" => (Brightness, 1),
        "darkens" | "darken" | "darkening" => (Brightness, -1),
        "warms" | "warm" | "warming" => (Warmth, 1),
        "cools" | "cool" | "cooling" => (Warmth, -1),
        "saturates" | "saturate" | "saturating" => (Saturation, 1),
        "desaturates" | "desaturate" | "desaturating" => (Saturation, -1),
        _ => return None,
    })
}

fn intransitive(w: &str) -> Option<(Dim, i8)> {
    use Dim::*;
    Some(match w {
        "brightens" | "brighten" => (Brightness, 1),
        "darkens" | "darken" => (Brightness, -1),
        "warms" | "warm" => (Warmth, 1),
        "cools" | "cool" => (Warmth, -1),
        _ => return None,
    })
}

fn up_down_verb(w: &str) -> Option<i8> {
    match w {
        "lifts" | "lift" | "lifting" | "raises" | "raise" | "raising" | "boosts" | "boost" | "boosting" | "increases" | "increase" | "increasing" => Some(1),
        "lowers" | "lower" | "lowering" | "reduces" | "reduce" | "reducing" | "cuts" | "cut" | "cutting" | "decreases" | "decrease" | "decreasing"
        | "pulls" | "pull" | "pulling" => Some(-1),
        _ => None,
    }
}

fn quantity_verb(w: &str) -> Option<i8> {
    match w {
        "rises" | "rise" | "increases" | "increase" => Some(1),
        "drops" | "drop" | "falls" | "fall" | "decreases" | "decrease" => Some(-1),
        _ => None,
    }
}

fn hue(w: &str) -> Option<(Dim, i8)> {
    Some(match w {
        "warm" | "orange" | "yellow" => (Dim::Warmth, 1),
        "cool" | "blue" => (Dim::Warmth, -1),
        "green" => (Dim::Tint, -1),
        "magenta" => (Dim::Tint, 1),
        _ => return None,
    })
}

fn number_word(w: &str) -> Option<f64> {
    Some(match w {
        "zero" => 0.0,
        "one" => 1.0,
        "two" => 2.0,
        "three" => 3.0,
        "four" => 4.0,
        "five" => 5.0,
        "six" => 6.0,
        "seven" => 7.0,
        "eight" => 8.0,
        "nine" => 9.0,
        "ten" => 10.0,
        "eleven" => 11.0,
        "twelve" => 12.0,
        "thirteen" => 13.0,
        "fourteen" => 14.0,
        "fifteen" => 15.0,
        "sixteen" => 16.0,
        "seventeen" => 17.0,
        "eighteen" => 18.0,
        "nineteen" => 19.0,
        "twenty" => 20.0,
        "thirty" => 30.0,
        "forty" => 40.0,
        "fifty" => 50.0,
        "sixty" => 60.0,
        "seventy" => 70.0,
        "eighty" => 80.0,
        "ninety" => 90.0,
        "hundred" => 100.0,
        _ => return None,
    })
}

fn fraction_word(w: &str) -> Option<f64> {
    Some(match w {
        "half" => 2.0,
        "third" | "thirds" => 3.0,
        "quarter" | "quarters" => 4.0,
        "fifth" | "fifths" => 5.0,
        "sixth" | "sixths" => 6.0,
        "eighth" | "eighths" => 8.0,
        _ => return None,
    })
}

const ADD: &[&str] = &["adds", "add", "adding"];
const LEAVE: &[&str] = &["leaves", "leave", "leaving"];
const BE: &[&str] = &["is", "are", "stay", "stays", "remain", "remains"];
const COPULA: &[&str] = &["get", "gets", "become", "becomes", "turn", "turns", "go", "goes", "is", "are", "look", "looks"];
const WEAK: &[&str] = &["slightly", "gently", "barely", "subtly"];
const STRONG: &[&str] = &["much", "strongly", "dramatically", "significantly", "heavily"];
const APPROX: &[&str] = &["about", "roughly", "around"];
const FRAME: &[&str] = &["frame", "picture", "image"];

// ---- the parser -----------------------------------------------------------------------------

/// A recursive-descent parser of the grammar in [`GRAMMAR_DOC`]: every function takes a token
/// index and returns the index after what it read, or None. Alternatives are tried in order;
/// claims pushed by a failed alternative are rolled back ([`Parser::attempt`]). The nesting is
/// fixed (no recursion), every loop advances, and the text is at most [`MAX_CHARS`] long.
struct Parser<'a> {
    t: &'a [Tok],
    /// The furthest token looked at (for the error position).
    far: usize,
    claims: Vec<Claim>,
    tools: Vec<Tool>,
    /// The first claim of the current sentence (an ellipsis only repeats a verb of its sentence).
    sentence_start: usize,
}

fn parse(toks: &[Tok]) -> Result<Parsed, FactMismatch> {
    let mut p = Parser { t: toks, far: 0, claims: Vec::new(), tools: Vec::new(), sentence_start: 0 };
    match p.text() {
        Some(()) if !p.claims.is_empty() => Ok(Parsed { claims: p.claims, tools: p.tools }),
        Some(()) => Err(FactMismatch::NoClaim("the text makes no claim".into())),
        None => {
            let at = p.far.min(toks.len());
            let near = match toks.get(at) {
                Some(Tok::Word(w)) => w.chars().take(20).collect(),
                Some(Tok::Num { v, .. }) => format!("{v}"),
                Some(Tok::Pct) => "%".into(),
                Some(Tok::Deg) => "°".into(),
                Some(Tok::P(c)) => c.to_string(),
                None => "the end".into(),
            };
            Err(FactMismatch::Ungrammatical(at, near))
        }
    }
}

impl<'a> Parser<'a> {
    fn tok(&mut self, i: usize) -> Option<&'a Tok> {
        self.far = self.far.max(i);
        self.t.get(i)
    }

    fn w(&mut self, i: usize) -> Option<&'a str> {
        match self.tok(i) {
            Some(Tok::Word(w)) => Some(w.as_str()),
            _ => None,
        }
    }

    fn kw(&mut self, i: usize, w: &str) -> Option<usize> {
        (self.w(i)? == w).then_some(i + 1)
    }

    /// A sequence of words.
    fn kws(&mut self, i: usize, ws: &[&str]) -> Option<usize> {
        let mut j = i;
        for w in ws {
            j = self.kw(j, w)?;
        }
        Some(j)
    }

    fn one_of(&mut self, i: usize, ws: &[&str]) -> Option<(&'a str, usize)> {
        let w = self.w(i)?;
        ws.contains(&w).then_some((w, i + 1))
    }

    fn p(&mut self, i: usize, c: char) -> Option<usize> {
        matches!(self.tok(i), Some(Tok::P(x)) if *x == c).then_some(i + 1)
    }

    fn num(&mut self, i: usize) -> Option<(f64, i8, usize)> {
        match self.tok(i) {
            Some(Tok::Num { v, sign }) => Some((*v, *sign, i + 1)),
            _ => None,
        }
    }

    /// Run an alternative; on failure, forget the claims and tools it pushed.
    fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let (c, t) = (self.claims.len(), self.tools.len());
        let r = f(self);
        if r.is_none() {
            self.claims.truncate(c);
            self.tools.truncate(t);
        }
        r
    }

    fn push(&mut self, scope: Scope, reg: Region, what: What, at: usize) -> Option<()> {
        let region = reg.merge(scope.region)?;
        self.claims.push(Claim { scope, region, what, at });
        Some(())
    }

    // text = sentence , [ sentence ] ; sentence = [ label ] , body , [ "." | "!" ]
    fn text(&mut self) -> Option<()> {
        let mut i = 0;
        while i < self.t.len() {
            self.sentence_start = self.claims.len();
            let mut region = Region::NONE;
            let j = self.body(i, &mut region)?;
            match self.tok(j) {
                None => return Some(()),
                Some(Tok::P('.' | '!')) => i = j + 1,
                Some(_) => return None,
            }
        }
        Some(())
    }

    fn body(&mut self, i: usize, region: &mut Region) -> Option<usize> {
        let mut i = self.clause(i, region, Join::Start)?;
        while let Some((j, join)) = self.joiner(i) {
            i = self.clause(j, region, join)?;
        }
        Some(i)
    }

    fn joiner(&mut self, i: usize) -> Option<(usize, Join)> {
        if let Some(j) = self.p(i, ';') {
            return Some((j, Join::Semicolon));
        }
        let j = self.p(i, ',').unwrap_or(i);
        if let Some((w, k)) = self.one_of(j, &["and", "while", "but"]) {
            return Some((k, if w == "and" { Join::List } else { Join::Contrast }));
        }
        (j > i).then_some((j, Join::List))
    }

    fn clause(&mut self, i: usize, region: &mut Region, join: Join) -> Option<usize> {
        let after_semicolon = join == Join::Semicolon;
        let i = match self.region_lead(i) {
            Some((r, j)) => {
                *region = r;
                j
            }
            None => i,
        };
        let reg = *region;
        if let Some(j) = self.attempt(|p| p.coverage(i)) {
            return Some(j);
        }
        if after_semicolon && let Some(j) = self.kw(i, "also") {
            return self.attempt(|p| p.texture(j, reg));
        }
        if let Some(j) = self.attempt(|p| p.verb_clause(i, reg)) {
            return Some(j);
        }
        if let Some(j) = self.attempt(|p| p.scoped_clause(i, reg, join == Join::List)) {
            return Some(j);
        }
        if let Some(j) = self.attempt(|p| p.quantity_clause(i, reg)) {
            return Some(j);
        }
        self.attempt(|p| p.predicates(i, None, reg))
    }

    // region_lead = region , [ region ] , [ "," ]
    fn region_lead(&mut self, i: usize) -> Option<(Region, usize)> {
        let (mut r, mut j) = self.region(i, true)?;
        if let Some((r2, k)) = self.region(j, false) {
            r = r.merge(r2)?;
            j = k;
        }
        Some((r, self.p(j, ',').unwrap_or(j)))
    }

    fn region(&mut self, i: usize, allow_it: bool) -> Option<(Region, usize)> {
        let (w, j) = self.one_of(i, &["inside", "in", "outside"])?;
        if allow_it
            && w == "inside"
            && let Some(k) = self.kw(j, "it")
        {
            // "inside it": the area of a share-of-the-frame claim earlier in the sentence
            let after_coverage = self.claims.get(self.sentence_start..)?.iter().any(|c| matches!(c.what, What::Coverage(_)));
            return after_coverage.then_some((Region::AREA, k));
        }
        let (mut r, k) = self.region_np(j)?;
        if w == "outside" {
            if r.subject.is_some() {
                return None;
            }
            r.window = r.window.map(|_| true);
            r.selection = r.selection.map(|_| true);
        }
        Some((r, k))
    }

    fn region_np(&mut self, i: usize) -> Option<(Region, usize)> {
        let j = self.kw(i, "the")?;
        if let Some(k) = self.kw(j, "window") {
            return Some((Region { window: Some(false), ..Region::AREA }, k));
        }
        if let Some(k) = self.kw(j, "selection").or_else(|| self.kws(j, &["qualifier", "selection"])) {
            return Some((Region { selection: Some(false), ..Region::AREA }, k));
        }
        let (selected, j) = match self.kw(j, "selected") {
            Some(k) => (true, k),
            None => (false, j),
        };
        let (subject, k) = if let Some(k) = self.kw(j, "skin") {
            (Subject::Skin, self.kw(k, "tones").unwrap_or(k))
        } else if let Some(k) = self.kw(j, "sky") {
            (Subject::Sky, k)
        } else {
            (Subject::Skin, self.one_of(j, &["face", "faces"])?.1)
        };
        Some((Region { subject: Some(subject), selection: selected.then_some(false), window: None, area: true }, k))
    }

    fn frame(&mut self, i: usize) -> Option<usize> {
        self.one_of(i, FRAME).map(|x| x.1)
    }

    // coverage = [ agent ] , "affects" , ( "the whole" frame | percent "of the" frame )
    fn coverage(&mut self, i: usize) -> Option<usize> {
        let i = self.agent(i).map_or(i, |x| x.1);
        let (_, j) = self.one_of(i, &["affects", "affect"])?;
        if let Some(k) = self.kw(j, "the") {
            let (_, k) = self.one_of(k, &["whole", "entire"])?;
            let k = self.frame(k)?;
            self.push(IMPLICIT, Region::NONE, What::Coverage(None), i)?;
            return Some(k);
        }
        let (a, k) = self.amount(j)?;
        if a.unit != Unit::Percent {
            return None;
        }
        let k = self.kws(k, &["of", "the"])?;
        let k = self.frame(k)?;
        self.push(IMPLICIT, Region::NONE, What::Coverage(Some(a)), i)?;
        Some(k)
    }

    /// The subject of a verb clause and the region it implies: the window or the qualifier act
    /// inside their (not inverted) matte.
    fn agent(&mut self, i: usize) -> Option<(Region, usize)> {
        if let Some(j) = self.kw(i, "it").or_else(|| self.kws(i, &["this", "node"])) {
            return Some((Region::NONE, j));
        }
        let j = self.kw(i, "the")?;
        let (w, k) = self.one_of(j, &["node", "grade", "curve", "lut", "window", "selection", "qualifier"])?;
        let window = Region { window: Some(false), ..Region::AREA };
        let selection = Region { selection: Some(false), ..Region::AREA };
        let (tool, region, k) = match w {
            "curve" => (Some(Tool::Curve), Region::NONE, k),
            "lut" => (Some(Tool::Lut), Region::NONE, k),
            "window" => (Some(Tool::Window), window, k),
            "selection" => (Some(Tool::Qualifier), selection, k),
            "qualifier" => (Some(Tool::Qualifier), selection, self.kw(k, "selection").unwrap_or(k)),
            _ => (None, Region::NONE, k),
        };
        self.tools.extend(tool);
        Some((region, k))
    }

    fn verb_clause(&mut self, i: usize, reg: Region) -> Option<usize> {
        let (r, j) = self.agent(i).unwrap_or((Region::NONE, i));
        self.verb_phrase(j, reg.merge(r)?)
    }

    fn strength(&mut self, i: usize) -> Option<(Strength, usize)> {
        if let Some((_, j)) = self.one_of(i, WEAK) {
            return Some((Strength::Weak, j));
        }
        if let Some((_, j)) = self.one_of(i, STRONG) {
            return Some((Strength::Strong, j));
        }
        let j = self.kw(i, "a")?;
        let (w, k) = self.one_of(j, &["little", "bit", "lot"])?;
        Some((if w == "lot" { Strength::Strong } else { Strength::Weak }, k))
    }

    fn strength_opt(&mut self, i: usize) -> (Option<Strength>, usize) {
        match self.strength(i) {
            Some((s, j)) => (Some(s), j),
            None => (None, i),
        }
    }

    fn verb_phrase(&mut self, i: usize, reg: Region) -> Option<usize> {
        let (pre, i) = self.strength_opt(i);
        if pre.is_none() {
            if let Some(j) = self.attempt(|p| p.texture(i, reg)) {
                return Some(j);
            }
            if let Some((_, j)) = self.one_of(i, LEAVE) {
                let (s, j) = self.ranges_scope(j)?;
                let k = self.still(j)?;
                self.push(s, reg, What::Untouched, i)?;
                return Some(k);
            }
        }
        let w = self.w(i)?;
        let j = i + 1;
        if let Some((dim, sign)) = dim_verb(w) {
            let (s, k) = self.scope(j).unwrap_or((IMPLICIT, j));
            return self.tail(k, i, dim, sign, s, pre, reg);
        }
        if let Some(sign) = up_down_verb(w) {
            if let Some((dim, k)) = self.quantity(j) {
                let (s, k) = self.in_scope(k)?;
                return self.tail(k, i, dim, sign, s, pre, reg);
            }
            let (s, k) = self.scope(j)?;
            return self.tail(k, i, Dim::Brightness, sign, s, pre, reg);
        }
        if ADD.contains(&w) {
            if let Some((dim, k)) = self.quantity(j) {
                let (s, k) = self.in_scope(k)?;
                return self.tail(k, i, dim, 1, s, pre, reg);
            }
            if let Some(k) = self.attempt(|p| p.tint(j, i, pre, reg)) {
                return Some(k);
            }
            let (a, k) = self.amount(j)?;
            let k = self.kw(k, "to")?;
            let (s, k) = self.scope(k)?;
            let what = What::Change { dim: Dim::Brightness, sign: 1, strength: pre.unwrap_or(Strength::Normal), amount: Some(a) };
            self.push(s, reg, what, i)?;
            return Some(k);
        }
        None
    }

    // "a" , [ tint_strength ] , hue , ( "tint" | "cast" ) , [ ( "to" | "in" ) , scope ]
    fn tint(&mut self, j: usize, at: usize, pre: Option<Strength>, reg: Region) -> Option<usize> {
        let k = self.kw(j, "a")?;
        let (st, k) = match self.one_of(k, &["slight", "subtle", "gentle", "strong"]) {
            Some((w, m)) => (Some(if w == "strong" { Strength::Strong } else { Strength::Weak }), m),
            None => (None, k),
        };
        if pre.is_some() && st.is_some() {
            return None;
        }
        let ((dim, sign), k) = {
            let w = self.w(k)?;
            (hue(w)?, k + 1)
        };
        let (_, k) = self.one_of(k, &["tint", "cast"])?;
        let (s, k) = match self.one_of(k, &["to", "in"]) {
            Some((_, m)) => self.scope(m)?,
            None => (IMPLICIT, k),
        };
        self.push(s, reg, What::Change { dim, sign, strength: pre.or(st).unwrap_or(Strength::Normal), amount: None }, at)?;
        Some(k)
    }

    fn texture(&mut self, i: usize, reg: Region) -> Option<usize> {
        let w = self.w(i)?;
        let (tex, k) = if ADD.contains(&w) {
            (Texture::Grain, self.kw(i + 1, "grain")?)
        } else if matches!(w, "reduces" | "reduce" | "reducing" | "removes" | "remove" | "removing") {
            (Texture::NoiseReduction, self.kw(i + 1, "noise")?)
        } else {
            return None;
        };
        let (s, k) = match self.one_of(k, &["to", "in"]) {
            Some((_, m)) => self.scope(m)?,
            None => (IMPLICIT, k),
        };
        self.push(s, reg, What::Texture(tex), i)?;
        Some(k)
    }

    /// tail = [ strength ] , [ "by" , amount ] (one strength per claim), then the claim.
    #[allow(clippy::too_many_arguments)]
    fn tail(&mut self, k: usize, at: usize, dim: Dim, sign: i8, scope: Scope, pre: Option<Strength>, reg: Region) -> Option<usize> {
        let (post, k) = if pre.is_none() { self.strength_opt(k) } else { (None, k) };
        let (amount, k) = self.by_amount(k);
        if dim == Dim::Contrast && scope != IMPLICIT {
            return None;
        }
        self.push(scope, reg, What::Change { dim, sign, strength: pre.or(post).unwrap_or(Strength::Normal), amount }, at)?;
        Some(k)
    }

    fn by_amount(&mut self, k: usize) -> (Option<Amount>, usize) {
        match self.kw(k, "by").and_then(|m| self.amount(m)) {
            Some((a, m)) => (Some(a), m),
            None => (None, k),
        }
    }

    fn in_scope(&mut self, k: usize) -> Option<(Scope, usize)> {
        match self.kw(k, "in") {
            Some(m) => self.scope(m),
            None => Some((IMPLICIT, k)),
        }
    }

    fn quantity(&mut self, i: usize) -> Option<(Dim, usize)> {
        let i = self.kw(i, "the").unwrap_or(i);
        let (w, j) = self.one_of(i, &["saturation", "contrast", "warmth", "brightness", "exposure"])?;
        let dim = match w {
            "saturation" => Dim::Saturation,
            "contrast" => Dim::Contrast,
            "warmth" => Dim::Warmth,
            _ => Dim::Brightness,
        };
        Some((dim, j))
    }

    fn still(&mut self, i: usize) -> Option<usize> {
        if let Some((_, j)) = self.one_of(i, &["untouched", "unchanged", "alone"]) {
            return Some(j);
        }
        self.kws(i, &["the", "same"])
    }

    /// `ellipsis`: the clause follows "," / "and", so "the midtones by half a stop" may repeat
    /// the verb of the claim right before it (not after "but", "while" or ";").
    fn scoped_clause(&mut self, i: usize, reg: Region, ellipsis: bool) -> Option<usize> {
        let (s, j) = self.scope(i)?;
        let named = matches!(s.span, Span::Ranges(_));
        if let Some(k) = self.kw(j, "by") {
            let (a, k) = self.amount(k)?;
            let last = self.claims.get(self.sentence_start..)?.last()?;
            let What::Change { dim, sign, .. } = last.what else { return None };
            if !named || !ellipsis || dim == Dim::Contrast {
                return None;
            }
            self.push(s, reg, What::Change { dim, sign, strength: Strength::Normal, amount: Some(a) }, i)?;
            return Some(k);
        }
        let b = self.one_of(j, BE).map_or(j, |x| x.1);
        if named && let Some(k) = self.still(b) {
            self.push(s, reg, What::Untouched, i)?;
            return Some(k);
        }
        if let Some(k) = self.attempt(|p| p.intransitive(j, s, reg)) {
            return Some(k);
        }
        let j = self.one_of(j, COPULA).map_or(j, |x| x.1);
        self.predicates(j, Some(s), reg)
    }

    fn intransitive(&mut self, j: usize, s: Scope, reg: Region) -> Option<usize> {
        let (pre, j) = self.strength_opt(j);
        let w = self.w(j)?;
        if matches!(w, "change" | "changes") {
            let (st, k) = self.strength(j + 1)?;
            if pre.is_some() || st != Strength::Weak {
                return None;
            }
            self.push(s, reg, What::ChangedSlightly, j)?;
            return Some(k);
        }
        let (dim, sign) = intransitive(w)?;
        self.tail(j + 1, j, dim, sign, s, pre, reg)
    }

    fn quantity_clause(&mut self, i: usize, reg: Region) -> Option<usize> {
        let (dim, j) = self.quantity(i)?;
        let (s, j) = self.in_scope(j)?;
        let sign = quantity_verb(self.w(j)?)?;
        self.tail(j + 1, i, dim, sign, s, None, reg)
    }

    // predicates = predicate , { sep , predicate } , [ "in" , scope ]
    fn predicates(&mut self, i: usize, lead: Option<Scope>, reg: Region) -> Option<usize> {
        let mut items = Vec::new();
        let (first, mut j) = self.predicate(i)?;
        items.push((first, i));
        loop {
            let k = self.p(j, ',').unwrap_or(j);
            let k = self.kw(k, "and").unwrap_or(k);
            if k == j {
                break;
            }
            match self.predicate(k) {
                Some((w, m)) => {
                    items.push((w, k));
                    j = m;
                }
                None => break,
            }
        }
        let (s, j) = match lead {
            Some(s) => (s, j),
            None => match self.kw(j, "in").and_then(|m| self.scope(m)) {
                Some(x) => x,
                None => (IMPLICIT, j),
            },
        };
        for (what, at) in items {
            if matches!(what, What::Change { dim: Dim::Contrast, .. }) && s != IMPLICIT {
                return None;
            }
            self.push(s, reg, what, at)?;
        }
        Some(j)
    }

    fn predicate(&mut self, i: usize) -> Option<(What, usize)> {
        use What::Change;
        // "+⅙ stop" (telegraphic, the sign is the direction)
        if let Some((v, sign, j)) = self.num(i)
            && sign != 0
            && let Some((_, k)) = self.one_of(j, &["stop", "stops"])
        {
            let amount = Some(Amount { v, sign, unit: Unit::Stops, below: false });
            return Some((Change { dim: Dim::Brightness, sign, strength: Strength::Normal, amount }, k));
        }
        if let Some(j) = self.kw(i, "saturation") {
            let (v, sign, j) = self.num(j)?;
            let k = self.pct_unit(j)?;
            if sign == 0 {
                return None;
            }
            let amount = Some(Amount { v, sign, unit: Unit::Percent, below: false });
            return Some((Change { dim: Dim::Saturation, sign, strength: Strength::Normal, amount }, k));
        }
        if let Some(j) = self.kws(i, &["changed", "slightly"]) {
            return Some((What::ChangedSlightly, j));
        }
        if let Some((w, j)) = self.one_of(i, &["more", "less"])
            && let Some(k) = self.kw(j, "contrast")
        {
            let sign = if w == "more" { 1 } else { -1 };
            return Some((Change { dim: Dim::Contrast, sign, strength: Strength::Normal, amount: None }, k));
        }
        if let Some((a, j)) = self.amount(i)
            && let Some(((dim, sign), k)) = self.adjective(j)
        {
            return Some((Change { dim, sign, strength: Strength::Normal, amount: Some(a) }, k));
        }
        let (strength, j) = self.strength_opt(i);
        let ((dim, sign), j) = self.adjective(j)?;
        let (amount, j) = self.by_amount(j);
        Some((Change { dim, sign, strength: strength.unwrap_or(Strength::Normal), amount }, j))
    }

    fn adjective(&mut self, i: usize) -> Option<((Dim, i8), usize)> {
        use Dim::*;
        let w = self.w(i)?;
        let single = match w {
            "brighter" => Some((Brightness, 1)),
            "darker" => Some((Brightness, -1)),
            "warmer" | "yellower" => Some((Warmth, 1)),
            "cooler" | "bluer" => Some((Warmth, -1)),
            "greener" => Some((Tint, -1)),
            "muted" | "desaturated" => Some((Saturation, -1)),
            _ => None,
        };
        if let Some(d) = single {
            return Some((d, i + 1));
        }
        let sign = match w {
            "more" => 1,
            "less" => -1,
            _ => return None,
        };
        match self.w(i + 1)? {
            "saturated" | "colorful" | "vivid" => Some(((Saturation, sign), i + 2)),
            "magenta" if sign > 0 => Some(((Tint, 1), i + 2)),
            _ => None,
        }
    }

    // scope = ( ranges | whole | region_np | "it" | [ "the" ] "colors" ) , [ region ]
    fn scope(&mut self, i: usize) -> Option<(Scope, usize)> {
        let (mut s, j) = self.core_scope(i)?;
        if let Some((r, k)) = self.region(j, false) {
            s.region = s.region.merge(r)?;
            return Some((s, k));
        }
        Some((s, j))
    }

    fn ranges_scope(&mut self, i: usize) -> Option<(Scope, usize)> {
        let (set, j) = self.ranges(i)?;
        Some((Scope { span: Span::Ranges(set), region: Region::NONE }, j))
    }

    fn core_scope(&mut self, i: usize) -> Option<(Scope, usize)> {
        if let Some(x) = self.ranges_scope(i) {
            return Some(x);
        }
        let tonal = |whole_frame: bool| Scope { span: Span::Tonal { whole_frame }, region: Region::NONE };
        if let Some(j) = self.kw(i, "everything").or_else(|| self.kw(i, "it")) {
            return Some((tonal(false), j));
        }
        if let Some(j) = self.kw(i, "all") {
            let j = self.kw(j, "the").unwrap_or(j);
            return Some((tonal(false), self.one_of(j, &["tones", "ranges"])?.1));
        }
        let (the, j) = match self.kw(i, "the") {
            Some(j) => (true, j),
            None => (false, i),
        };
        if let Some(k) = self.kw(j, "colors") {
            return Some((tonal(false), k));
        }
        if !the {
            return None;
        }
        if let Some(k) = self.frame(j) {
            return Some((tonal(false), k));
        }
        if let Some((_, k)) = self.one_of(j, &["whole", "entire"]) {
            return Some((tonal(true), self.frame(k)?));
        }
        let (r, k) = self.region_np(i)?;
        Some((Scope { span: Span::Area, region: r }, k))
    }

    /// A list of distinct ranges. A bare comma joins only when an "and" follows later in the
    /// list ("shadows, midtones and highlights"); otherwise it ends the scope ("the midtones,
    /// the shadows get …" is two clauses).
    fn ranges(&mut self, i: usize) -> Option<([bool; 3], usize)> {
        let (r, mut j) = self.range(i)?;
        // (range, end, joined with "and")
        let mut items = vec![(r, j, true)];
        loop {
            let k = self.p(j, ',').unwrap_or(j);
            let (and, k) = match self.kw(k, "and") {
                Some(m) => (true, m),
                None => (false, k),
            };
            if k == j {
                break;
            }
            let Some((r, m)) = self.range(k) else { break };
            items.push((r, m, and));
            j = m;
        }
        let last_and = items.iter().rposition(|x| x.2).unwrap_or(0);
        let mut set = [false; 3];
        let mut end = i;
        for (r, m, _) in items.iter().take(last_and + 1) {
            match set.get_mut(*r) {
                Some(s) if !*s => *s = true,
                _ => return None,
            }
            end = *m;
        }
        Some((set, end))
    }

    fn range(&mut self, i: usize) -> Option<(usize, usize)> {
        let i = self.kw(i, "the").unwrap_or(i);
        let (w, j) = self.one_of(i, &RANGE_NAMES)?;
        Some((RANGE_NAMES.iter().position(|n| *n == w)?, j))
    }

    fn pct_unit(&mut self, i: usize) -> Option<usize> {
        if matches!(self.tok(i), Some(Tok::Pct)) {
            return Some(i + 1);
        }
        self.kw(i, "percent")
    }

    fn unit(&mut self, j: usize) -> Option<(Unit, usize)> {
        if let Some(k) = self.pct_unit(j) {
            return Some((Unit::Percent, k));
        }
        if let Some((_, k)) = self.one_of(j, &["stop", "stops"]) {
            return Some((Unit::Stops, k));
        }
        self.kws(j, &["of", "a", "stop"]).map(|k| (Unit::Stops, k))
    }

    fn word_number(&mut self, i: usize) -> Option<(f64, usize)> {
        let w = self.w(i)?;
        Some((number_word(w)?, i + 1))
    }

    // number_words = number_word , [ "-" , number_word ] , [ "point" , number_word ] | "point" , number_word
    fn number_words(&mut self, i: usize) -> Option<(f64, usize)> {
        let digit = |p: &mut Self, k: usize| p.word_number(k).filter(|(d, _)| *d < 10.0);
        if let Some(j) = self.kw(i, "point") {
            let (d, k) = digit(self, j)?;
            return Some((d / 10.0, k));
        }
        let (mut n, mut j) = self.word_number(i)?;
        if (20.0..100.0).contains(&n)
            && let Some(k) = self.p(j, '-')
        {
            let (m, k) = digit(self, k).filter(|(m, _)| *m >= 1.0)?;
            n += m;
            j = k;
        }
        if let Some(k) = self.kw(j, "point") {
            let (d, k) = digit(self, k)?;
            n += d / 10.0;
            j = k;
        }
        Some((n, j))
    }

    fn amount(&mut self, i: usize) -> Option<(Amount, usize)> {
        let i = self.one_of(i, APPROX).map_or(i, |x| x.1);
        if let Some(j) = self.p(i, '<') {
            let (v, sign, k) = self.num(j)?;
            let k = self.pct_unit(k)?;
            return (sign == 0).then_some((Amount { v, sign: 0, unit: Unit::Percent, below: true }, k));
        }
        if let Some((v, sign, j)) = self.num(i) {
            let (unit, k) = self.unit(j)?;
            return Some((Amount { v, sign, unit, below: false }, k));
        }
        if let Some(j) = self.kws(i, &["a", "stop"]) {
            return Some((Amount::stops(1.0), j));
        }
        if let Some(j) = self.kws(i, &["half", "a", "stop"]).or_else(|| self.kws(i, &["a", "half", "stop"])) {
            return Some((Amount::stops(0.5), j));
        }
        let numer = match self.kw(i, "a") {
            Some(j) => Some((1.0, j)),
            None => self.word_number(i),
        };
        if let Some((n, j)) = numer
            && let Some(d) = self.w(j).and_then(fraction_word)
            && let Some(k) = self.kws(j + 1, &["of", "a", "stop"])
        {
            return Some((Amount::stops(n / d), k));
        }
        let (v, j) = self.number_words(i)?;
        let (unit, k) = self.unit(j)?;
        Some((Amount { v, sign: 0, unit, below: false }, k))
    }
}

// ---- verification ---------------------------------------------------------------------------

fn within(claim: f64, fact: f64, tol: f64) -> bool {
    (claim - fact).abs() <= tol
}

fn pct_tol(fact: f64) -> f64 {
    TOL_PCT_POINTS.max(TOL_PCT_REL * fact.abs())
}

fn dim_name(d: Dim, sign: i8) -> &'static str {
    match (d, sign > 0) {
        (Dim::Brightness, true) => "brighter",
        (Dim::Brightness, false) => "darker",
        (Dim::Warmth, true) => "warmer",
        (Dim::Warmth, false) => "cooler",
        (Dim::Tint, true) => "more magenta",
        (Dim::Tint, false) => "greener",
        (Dim::Saturation, true) => "more saturated",
        (Dim::Saturation, false) => "less saturated",
        (Dim::Contrast, true) => "more contrast",
        (Dim::Contrast, false) => "less contrast",
    }
}

fn range_name(r: usize) -> &'static str {
    RANGE_NAMES.get(r).copied().unwrap_or("range")
}

/// The level at which a change counts as strong (⅚ stop, a 0.08 cast, 40 % saturation, a stop
/// of contrast): 1.0.
fn level(dim: Dim, v: f64) -> f64 {
    match dim {
        Dim::Brightness => v.abs() / (5.0 / 6.0),
        Dim::Warmth | Dim::Tint => v.abs() / f64::from(CAST_STEPS[2]),
        Dim::Saturation => v.abs() / 40.0,
        Dim::Contrast => v.abs(),
    }
}

/// Weak words need a level below this (⅝ stop · 0.04 cast · 20 % saturation · ½ stop of contrast).
const WEAK_BELOW: f64 = 0.5;
/// Strong words need at least this level (⅔ stop · 0.064 cast · 32 % saturation · 0.8 stop of
/// contrast).
const STRONG_FROM: f64 = 0.8;
/// The smallest amounts that say something (a stated "0.01 stops" or "0 %" doesn't).
const MIN_STOPS: f64 = 1.0 / 12.0;
const MIN_PCT: f64 = 1.0;
/// "<N % of the frame" only for small N.
const MAX_BELOW_PCT: f64 = 5.0;

/// The facts, with the checks of claims against them.
struct Fx<'a> {
    f: &'a NodeFacts,
}

impl Fx<'_> {
    fn touched(&self, r: usize) -> bool {
        self.f.ranges.get(r).is_some_and(|f| f.touched)
    }

    fn val(&self, r: usize, dim: Dim) -> f64 {
        let Some(f) = self.f.ranges.get(r) else { return 0.0 };
        match dim {
            Dim::Brightness => f.stops as f64,
            Dim::Warmth => f.warmth as f64,
            Dim::Tint => f.tint as f64,
            Dim::Saturation => f.saturation.unwrap_or(0.0) as f64,
            Dim::Contrast => 0.0,
        }
    }

    /// Whether a fact is big enough that the template itself would mention it.
    fn passes(&self, dim: Dim, v: f64) -> bool {
        match dim {
            Dim::Brightness => sixths(v as f32) != 0,
            Dim::Warmth | Dim::Tint => cast_level(v as f32) >= 1,
            Dim::Saturation => sat_pct(&RangeFacts { saturation: Some(v as f32), ..RangeFacts::untouched("shadows") }).is_some(),
            Dim::Contrast => v.abs() >= 0.17,
        }
    }

    /// Highlights minus shadows exposure (stops), or the contrast control's direction.
    fn contrast_value(&self) -> Option<f64> {
        if self.touched(0) && self.touched(2) {
            return Some(self.val(2, Dim::Brightness) - self.val(0, Dim::Brightness));
        }
        self.f.controls.contrast.filter(|[k, _]| k.is_finite() && (*k - 1.0).abs() >= 0.05).map(|[k, _]| if k > 1.0 { 1.0 } else { -1.0 })
    }

    fn verify(&self, p: &Parsed) -> Result<(), FactMismatch> {
        for t in &p.tools {
            self.tool(*t)?;
        }
        for c in &p.claims {
            self.region(&c.region)?;
            self.claim(c)?;
        }
        self.dominant_covered(&p.claims)
    }

    fn tool(&self, t: Tool) -> Result<(), FactMismatch> {
        let c = &self.f.controls;
        let (ok, name) = match t {
            Tool::Curve => (!c.curves.is_empty(), "a curve"),
            Tool::Lut => (c.lut.is_some(), "a LUT"),
            Tool::Window => (c.windows > 0, "a window"),
            Tool::Qualifier => (c.qualifier.is_some(), "a qualifier"),
        };
        if ok { Ok(()) } else { Err(FactMismatch::Unsupported(format!("{name}, but the node has none"))) }
    }

    fn region(&self, r: &Region) -> Result<(), FactMismatch> {
        let c = &self.f.controls;
        let fail = |s: &str| Err(FactMismatch::Selection(s.to_string()));
        if let Some(outside) = r.window {
            if c.windows == 0 {
                return fail("a window, but the node has none");
            }
            if outside && c.windows_inverted != c.windows {
                return fail("outside the window, but the window isn't inverted");
            }
            if !outside && c.windows_inverted != 0 {
                return fail("inside the window, but the window is inverted");
            }
        }
        if let Some(outside) = r.selection {
            let Some(q) = &c.qualifier else { return fail("a selection, but the node has no qualifier") };
            if q.invert != outside {
                return fail(if outside { "outside the selection, but the qualifier isn't inverted" } else { "the selection, but the qualifier is inverted" });
            }
        }
        if let Some(subject) = r.subject {
            let Some(q) = c.qualifier.as_ref().filter(|q| !q.invert) else {
                return fail("skin, faces or sky need a (not inverted) qualifier that selects them");
            };
            let hues: &[&str] = match subject {
                Subject::Skin => &["red", "orange", "yellow", "rose"],
                Subject::Sky => &["blue", "azure", "cyan"],
            };
            if !hues.iter().any(|h| q.words.contains(&format!("around {h} ("))) {
                return fail("the qualifier doesn't select those hues");
            }
        }
        Ok(())
    }

    /// The ranges a scope speaks about; all of them must be touched.
    /// A whole-picture claim about a node that acts on part of the frame must say where.
    fn tonal_ok(&self, region: &Region) -> Result<(), FactMismatch> {
        let partial = self.f.matte_coverage.is_some() || self.f.coverage < WHOLE_FRAME;
        if partial && !region.area {
            return Err(FactMismatch::Unsupported(
                "the whole picture, but the node changes only part of the frame (say where: inside it, inside the window, in the selection)".into(),
            ));
        }
        Ok(())
    }

    fn ranges_of(&self, s: &Scope, region: &Region) -> Result<Vec<usize>, FactMismatch> {
        match s.span {
            Span::Ranges(set) => {
                let rs: Vec<usize> = (0..3).filter(|r| set.get(*r) == Some(&true)).collect();
                if let Some(r) = rs.iter().find(|r| !self.touched(**r)) {
                    return Err(FactMismatch::Range(format!("the text talks about the {}, which the node didn't change", range_name(*r))));
                }
                Ok(rs)
            }
            Span::Tonal { whole_frame } => {
                if whole_frame && !(self.f.matte_coverage.is_none() && self.f.coverage >= WHOLE_FRAME) {
                    return Err(FactMismatch::Unsupported("says the whole frame, but the node only changes part of it".into()));
                }
                self.tonal_ok(region)?;
                if let Some(r) = (0..3).find(|r| !self.touched(*r)) {
                    return Err(FactMismatch::Range(format!("the text speaks about the whole picture, but the {} are untouched", range_name(r))));
                }
                Ok(vec![0, 1, 2])
            }
            Span::Area => {
                let rs: Vec<usize> = (0..3).filter(|r| self.touched(*r)).collect();
                if rs.is_empty() {
                    return Err(FactMismatch::Range("nothing in the selected area changes".into()));
                }
                Ok(rs)
            }
        }
    }

    fn claim(&self, c: &Claim) -> Result<(), FactMismatch> {
        match c.what {
            What::Coverage(a) => self.coverage(a),
            What::Untouched => {
                let Span::Ranges(set) = c.scope.span else {
                    return Err(FactMismatch::Range("only named tonal ranges can be untouched".into()));
                };
                match (0..3).find(|r| set.get(*r) == Some(&true) && self.touched(*r)) {
                    Some(r) => Err(FactMismatch::Range(format!("the text says the {} are untouched, but the node changed them", range_name(r)))),
                    None => Ok(()),
                }
            }
            What::ChangedSlightly => {
                // only when no measured change reaches the template's threshold
                for r in self.ranges_of(&c.scope, &c.region)? {
                    for dim in [Dim::Brightness, Dim::Warmth, Dim::Tint, Dim::Saturation] {
                        let v = self.val(r, dim);
                        if self.passes(dim, v) {
                            return Err(FactMismatch::Intensity(format!(
                                "the {} change slightly, but they get {}",
                                range_name(r),
                                dim_name(dim, if v > 0.0 { 1 } else { -1 })
                            )));
                        }
                    }
                }
                Ok(())
            }
            What::Texture(t) => {
                let c2 = &self.f.controls;
                match t {
                    Texture::Grain if c2.grain.is_none() => return Err(FactMismatch::Unsupported("grain, but the node has none".into())),
                    Texture::NoiseReduction if c2.noise_reduction.is_none() => {
                        return Err(FactMismatch::Unsupported("noise reduction, but the node has none".into()));
                    }
                    _ => {}
                }
                self.ranges_of(&c.scope, &c.region).map(|_| ())
            }
            What::Change { dim, sign, strength, amount } => self.change(&c.scope, &c.region, dim, sign, strength, amount),
        }
    }

    fn coverage(&self, a: Option<Amount>) -> Result<(), FactMismatch> {
        let f = self.f;
        let Some(a) = a else {
            return if f.matte_coverage.is_none() && f.coverage >= WHOLE_FRAME {
                Ok(())
            } else {
                Err(FactMismatch::Unsupported("says the whole frame, but the node only affects part of it".into()))
            };
        };
        let aff = f.affected() as f64 * 100.0;
        let ok = a.unit == Unit::Percent
            && a.sign == 0
            && a.v <= 100.0
            && if a.below {
                a.v <= MAX_BELOW_PCT && aff > 0.0 && aff < a.v
            } else {
                a.v >= MIN_PCT
                    && [Some(aff), Some(f.coverage as f64 * 100.0), f.matte_coverage.map(|m| m as f64 * 100.0)]
                        .into_iter()
                        .flatten()
                        .any(|c| within(a.v, c, pct_tol(c)))
            };
        if ok { Ok(()) } else { Err(FactMismatch::Number(format!("affects {} % of the frame, but it affects {aff:.0} %", a.v))) }
    }

    fn change(&self, scope: &Scope, region: &Region, dim: Dim, sign: i8, strength: Strength, amount: Option<Amount>) -> Result<(), FactMismatch> {
        let said = dim_name(dim, sign);
        let vals: Vec<(Option<usize>, f64)> = if dim == Dim::Contrast {
            self.tonal_ok(region)?;
            self.contrast_value().map(|v| (None, v)).into_iter().collect()
        } else {
            self.ranges_of(scope, region)?.into_iter().map(|r| (Some(r), self.val(r, dim))).collect()
        };
        if vals.is_empty() {
            return Err(FactMismatch::Direction(format!("{said}: no measured change to support it")));
        }
        let where_ = |r: Option<usize>| r.map_or("the picture", range_name);
        for (r, v) in &vals {
            if !self.passes(dim, *v) {
                return Err(FactMismatch::Direction(format!("{said} ({}): the facts show no clear change there", where_(*r))));
            }
            if (*v > 0.0) != (sign > 0) {
                return Err(FactMismatch::Direction(format!("{said} ({}): the facts show the opposite", where_(*r))));
            }
            match strength {
                Strength::Weak if level(dim, *v) >= WEAK_BELOW => {
                    return Err(FactMismatch::Intensity(format!("{said} ({}) is called slight, but the change is strong", where_(*r))));
                }
                Strength::Strong if level(dim, *v) < STRONG_FROM => {
                    return Err(FactMismatch::Intensity(format!("{said} ({}) is called strong, but the change is small", where_(*r))));
                }
                _ => {}
            }
        }
        let Some(a) = amount else { return Ok(()) };
        if a.below {
            return Err(FactMismatch::Number("\"<\" only for the share of the frame".into()));
        }
        if a.sign != 0 && a.sign != sign {
            return Err(FactMismatch::Number(format!("a signed amount that disagrees with {said}")));
        }
        if a.v < if a.unit == Unit::Stops { MIN_STOPS } else { MIN_PCT } {
            return Err(FactMismatch::Number(format!("{} is too small an amount to say anything", a.v)));
        }
        let ok = |fact: f64, tol: f64| within(a.v, fact.abs(), tol);
        match (a.unit, dim) {
            (Unit::Stops, Dim::Brightness) => {
                for (r, v) in &vals {
                    if !ok(*v, TOL_STOPS.max(TOL_STOPS_REL * v.abs())) {
                        return Err(FactMismatch::Number(format!("{} stops ({}), but the facts show {:.2}", a.v, where_(*r), v)));
                    }
                }
                Ok(())
            }
            (Unit::Percent, Dim::Saturation) => {
                for (r, _) in &vals {
                    let fact = r.and_then(|r| self.f.ranges.get(r)).and_then(|f| f.saturation).map(f64::from);
                    if !fact.is_some_and(|s| ok(s, pct_tol(s))) {
                        return Err(FactMismatch::Number(format!("{} % saturation ({}) doesn't match the measured change", a.v, where_(*r))));
                    }
                }
                Ok(())
            }
            (Unit::Percent, Dim::Contrast) => {
                let fact = self.f.controls.contrast.map(|[k, _]| (k as f64 - 1.0) * 100.0);
                if fact.is_some_and(|c| ok(c, pct_tol(c))) {
                    Ok(())
                } else {
                    Err(FactMismatch::Number(format!("{} % contrast doesn't match the contrast control", a.v)))
                }
            }
            (unit, _) => Err(FactMismatch::Number(format!("{} {} can't state a {said} change", a.v, if unit == Unit::Stops { "stops" } else { "percent" }))),
        }
    }

    /// Step 6: some change with at least half the level of the largest one is covered.
    fn dominant_covered(&self, claims: &[Claim]) -> Result<(), FactMismatch> {
        let mut changes = Vec::new();
        for r in (0..3).filter(|r| self.touched(*r)) {
            for dim in [Dim::Brightness, Dim::Warmth, Dim::Tint, Dim::Saturation] {
                let v = self.val(r, dim);
                if self.passes(dim, v) {
                    changes.push((r, dim, v, level(dim, v)));
                }
            }
        }
        let Some(max) = changes.iter().map(|c| c.3).max_by(f64::total_cmp) else { return Ok(()) };
        let covers = |s: &Scope, r: usize| match s.span {
            Span::Ranges(set) => set.get(r) == Some(&true),
            Span::Tonal { .. } | Span::Area => true,
        };
        let covered = |r: usize, dim: Dim| {
            claims.iter().any(|c| match c.what {
                What::Change { dim: d, .. } => d == dim && covers(&c.scope, r),
                _ => false,
            })
        };
        // every change with at least half the level of the largest one (contrast doesn't stand in
        // for brightness)
        match changes.iter().find(|(r, d, _, l)| *l * 2.0 >= max && !covered(*r, *d)) {
            Some((r, d, v, _)) => Err(FactMismatch::Omission(format!("the {} get {}", range_name(*r), dim_name(*d, if *v > 0.0 { 1 } else { -1 })))),
            None => Ok(()),
        }
    }
}

// ---- no-op nodes ----------------------------------------------------------------------------

fn phrase_eq(p: &[&str], seg: &[String]) -> bool {
    p.len() == seg.len() && p.iter().zip(seg).all(|(a, b)| a == b)
}

/// The text split at punctuation (and before "rather"), filler words dropped; numbers are "#".
fn noop_segments(toks: &[Tok]) -> Vec<Vec<String>> {
    let mut segs: Vec<Vec<String>> = vec![Vec::new()];
    for t in toks {
        let item = match t {
            Tok::Word(w) if NOOP_DROP.contains(&w.as_str()) => None,
            Tok::Word(w) => {
                if w == "rather" {
                    segs.push(Vec::new());
                }
                Some(w.clone())
            }
            Tok::Num { .. } | Tok::Pct | Tok::Deg => Some("#".to_string()),
            Tok::P(';' | ':' | ',' | '.' | '!' | '?' | '(' | ')') => {
                segs.push(Vec::new());
                None
            }
            Tok::P(_) => None,
        };
        if let (Some(item), Some(seg)) = (item, segs.last_mut()) {
            seg.push(item);
        }
    }
    segs.retain(|s| !s.is_empty());
    segs
}

/// Step 3: a node that doesn't change the picture may only use the phrasings for its state.
fn check_noop(f: &NodeFacts, toks: &[Tok]) -> Result<(), FactMismatch> {
    let mut allowed: Vec<&[&str]> = Vec::new();
    if f.showing_matte {
        allowed.extend(NOOP_PHRASES.get(MATTE_PHRASES).unwrap_or_default());
    } else {
        allowed.extend(NOOP_PHRASES.get(NOCHANGE_PHRASES).unwrap_or_default());
    }
    if !f.enabled {
        allowed.extend(NOOP_PHRASES.get(DISABLED_PHRASES).unwrap_or_default());
    }
    let segs = noop_segments(toks);
    if segs.is_empty() {
        return Err(FactMismatch::NoChange("the node doesn't change the picture, and the text doesn't say so".into()));
    }
    for seg in &segs {
        if !allowed.iter().any(|p| phrase_eq(p, seg)) {
            return Err(FactMismatch::NoChange(format!("\"{}\" is not a way to say the node does nothing", seg.join(" "))));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade_explain::{ControlFacts, QualifierFact};

    fn rf(range: &'static str, stops: f32, warmth: f32, tint: f32, sat: Option<f32>) -> RangeFacts {
        RangeFacts { range, touched: true, share: 0.9, stops, warmth, tint, saturation: sat }
    }

    fn facts(ranges: [RangeFacts; 3]) -> NodeFacts {
        NodeFacts { enabled: true, showing_matte: false, measured: true, controls: ControlFacts::default(), coverage: 0.96, matte_coverage: None, ranges }
    }

    /// Shadows +⅙ stop and slightly warmer; midtones +½ stop (the main change) and 11 % more
    /// saturated; highlights untouched.
    fn mixed() -> NodeFacts {
        facts([rf("shadows", 0.17, 0.02, 0.0, None), rf("midtones", 0.5, 0.0, 0.0, Some(11.0)), RangeFacts::untouched("highlights")])
    }

    fn even(stops: f32, warmth: f32) -> NodeFacts {
        facts([rf("shadows", stops, warmth, 0.0, None), rf("midtones", stops, warmth, 0.0, None), rf("highlights", stops, warmth, 0.0, None)])
    }

    fn ok(f: &NodeFacts, t: &str) {
        assert_eq!(check_explanation(f, t), Ok(()), "{t}");
    }

    fn bad(f: &NodeFacts, t: &str) {
        assert!(check_explanation(f, t).is_err(), "accepted: {t}");
    }

    #[test]
    fn shape_rules() {
        assert_eq!(check_shape("   "), Err(FactMismatch::Empty));
        assert!(matches!(check_shape(&"a ".repeat(200)), Err(FactMismatch::TooLong(_))));
        assert!(matches!(check_shape("One. Two. Three."), Err(FactMismatch::TooManySentences(3))));
        assert!(check_shape("One. Two!").is_ok() && check_shape("It adds 0.5 stops").is_ok());
        for b in ["**bold**", "- item", "1. item", "a\nb", "`code`", "# h", "see [x](y)"] {
            assert!(matches!(check_shape(b), Err(FactMismatch::Formatting(_))), "{b}");
        }
    }

    #[test]
    fn hostile_text_never_panics() {
        let f = mixed();
        let huge = "9".repeat(10_000);
        let long_list = "the shadows, ".repeat(30);
        for t in [
            "1e999 stops",
            "NaN stops",
            "inf %",
            "½½½ stop",
            "1/0 stop",
            "99999999999999999999999999999 %",
            "٣ stops",
            "１ stop",
            "−",
            "+",
            "%",
            "°",
            "1e",
            "1e+",
            "e5",
            "..",
            "",
            "\u{0}",
            "∞",
            "⅓ ⅔ ¼",
            "Ｎｏｄｅ",
            &huge,
            &long_list,
            "1/",
            "/1",
            "+-+-1",
            "node 1 'unclosed",
            "node 99999999999999999999 'x'",
            "a b c d e f g ",
            "1 ½ stops ½",
            "by by by by",
            "the the the",
            "and and, and; and",
            "< < 1 %",
            "twenty- point",
            "point",
            "a a a stop",
            "the midtones by",
            "; also",
            "inside it, inside it,",
        ] {
            let _ = check_explanation(&f, t);
            let _ = check_grammar(t);
        }
        assert!(matches!(check_explanation(&f, "adds 1e999 stops"), Err(FactMismatch::Numeral(_))));
        assert!(check_explanation(&f, "adds NaN stops").is_err());
        assert!(matches!(check_explanation(&f, "adds ٣ stops"), Err(FactMismatch::Numeral(_))));
    }

    #[test]
    fn the_grammar_accepts_plain_paraphrases() {
        for t in [
            "The shadows get warmer and brighter.",
            "The midtones get half a stop brighter.",
            "It brightens the midtones by about half a stop.",
            "Shadows +⅙ stop, slightly warmer; midtones +½ stop, saturation +20 %; highlights untouched.",
            "Node 3: affects 96 % of the frame; inside it, +⅓ stop in all tones.",
            "Node 3 'Skin': affects the whole frame.",
            "Saturation drops by 17 percent.",
            "It lowers contrast, brightening the shadows and darkening the highlights.",
            "The curve brightens the shadows by ⅔ stop, the midtones by half a stop and the highlights by a sixth of a stop.",
            "Outside the window, the midtones get slightly warmer and the shadows are untouched.",
            "In the selected skin tones inside the window, the midtones get brighter.",
            "It adds a slight magenta tint.",
            "It adds grain; also reduces noise.",
            "The highlights change slightly, while the shadows stay the same.",
            "Colors get twenty-seven percent more saturated.",
            "It brightens the picture by one point five stops.",
            "It leaves the highlights untouched.",
            "Disabled, no visible change.",
        ] {
            assert_eq!(check_grammar(t), Ok(()), "{t}");
        }
    }

    #[test]
    fn the_grammar_rejects_everything_else() {
        for t in [
            "The shadows get darker, also the highlights.",
            "It leaves the shadows brighter.",
            "Lowers shadows to a cool tint.",
            "The picture gets darker, slightly.",
            "The shadows get darker than the highlights.",
            "Brightens the midtones by 20 % with more color.",
            "It brightens the shadows then the midtones.",
            "It brightens the shadows so the midtones.",
            "Shadows ⅙ stop.",
            "Brighter brighter.",
            "The shadows, the midtones.",
            "It adds a tint.",
            "It removes the green tint.",
            "It brightens by 3.",
        ] {
            assert!(matches!(check_grammar(t), Err(FactMismatch::Ungrammatical(..) | FactMismatch::UnknownWord(_))), "{t}: {:?}", check_grammar(t));
        }
        // the position of the word where parsing got stuck
        assert_eq!(check_grammar("The shadows get darker than the highlights."), Err(FactMismatch::Ungrammatical(4, "than".into())));
    }

    #[test]
    fn numbers_belong_to_their_clause() {
        let f = mixed();
        ok(&f, "Shadows +⅙ stop, midtones +½ stop.");
        ok(&f, "The midtones get about half a stop brighter.");
        ok(&f, "The midtones get a third of a stop brighter.");
        bad(&f, "The midtones get one stop brighter.");
        bad(&f, "The midtones get a stop brighter.");
        bad(&f, "The midtones get three quarters of a stop brighter.");
        ok(&f, "Midtones saturation +11 %, half a stop brighter.");
        bad(&f, "Midtones saturation −11 %, half a stop brighter.");
        ok(&f, "It affects 96 % of the frame; the midtones get half a stop brighter.");
        bad(&f, "It affects 50 % of the frame; the midtones get half a stop brighter.");
        bad(&f, "It brightens the midtones by 50 percent.");
        bad(&f, "It brightens the midtones by a third.");
        // the ellipsis takes its own number
        let g = facts([rf("shadows", 0.33, 0.0, 0.0, None), RangeFacts::untouched("midtones"), rf("highlights", 1.0, 0.0, 0.0, None)]);
        ok(&g, "The highlights brighten by a stop, the shadows by ⅓ stop.");
        bad(&g, "The highlights brighten by ⅓ stop, the shadows by ⅓ stop.");
        bad(&g, "The highlights brighten by a stop, the shadows by a stop.");
    }

    #[test]
    fn directions_follow_the_facts() {
        let f = mixed();
        ok(&f, "The shadows get warmer, while the midtones get brighter and more saturated.");
        bad(&f, "The shadows get cooler, while the midtones get brighter.");
        bad(&f, "The shadows get darker and warmer, while the midtones get brighter.");
        bad(&f, "The midtones are less saturated, the shadows warmer.");
        ok(&f, "The midtones look more vivid and brighter, the shadows warmer.");
        bad(&f, "The shadows are brighter and the midtones are warmer.");
        bad(&f, "It raises contrast.");
        bad(&f, "The shadows get more magenta.");
        // the whole picture needs every range touched
        bad(&f, "The picture gets brighter.");
        bad(&f, "Everything gets brighter.");
        let c = facts([rf("shadows", -0.33, 0.0, 0.0, None), rf("midtones", 0.1, 0.0, 0.0, None), rf("highlights", 0.4, 0.0, 0.0, None)]);
        ok(&c, "It adds contrast, darkening the shadows and brightening the highlights.");
        bad(&c, "It adds contrast.");
        bad(&c, "It lowers contrast.");
        ok(&c, "The shadows get darker and the highlights brighter.");
        bad(&c, "The shadows get brighter and the highlights darker.");
        bad(&c, "Everything gets brighter.");
    }

    #[test]
    fn strength_words() {
        let weak = even(0.05, 0.012);
        ok(&weak, "Slightly warmer.");
        ok(&weak, "It warms the picture a little.");
        bad(&weak, "Much warmer.");
        bad(&weak, "It warms the picture a lot.");
        let strong = even(-1.5, 0.1);
        ok(&strong, "Much warmer and much darker.");
        bad(&strong, "Slightly warmer and much darker.");
        bad(&strong, "Much warmer and a little darker.");
        bad(&strong, "Much warmer and a bit darker.");
        bad(&strong, "Much warmer, gently darker.");
        bad(&strong, "It barely darkens the picture, much warmer.");
        bad(&strong, "The picture gets much warmer and darker, slightly.");
    }

    #[test]
    fn ranges_must_be_touched_or_left_alone() {
        let f = mixed();
        ok(&f, "The shadows get warmer and the midtones half a stop brighter; the highlights are untouched.");
        ok(&f, "The highlights stay the same, the midtones get brighter and the shadows warmer.");
        bad(&f, "The highlights get brighter.");
        bad(&f, "The shadows are untouched, the midtones brighter.");
        bad(&f, "The shadows and highlights are brighter.");
        ok(&f, "The shadows and midtones are brighter, the shadows warmer.");
    }

    #[test]
    fn the_dominant_change_must_be_mentioned() {
        let f = facts([rf("shadows", -3.0, 0.0, 0.0, None), rf("midtones", 0.0, 0.0, 0.0, None), rf("highlights", 0.0, 0.015, 0.0, None)]);
        assert!(matches!(check_explanation(&f, "The highlights get slightly warmer."), Err(FactMismatch::Omission(_))));
        ok(&f, "The shadows get much darker and the highlights slightly warmer.");
    }

    #[test]
    fn noop_nodes_may_only_say_so() {
        let mut off = mixed();
        off.enabled = false;
        for t in ["Disabled, no visible change.", "This node is switched off."] {
            ok(&off, t);
        }
        for t in ["Makes the shadows brighter.", "Disabled, but adds +⅙ stop.", "It looks the same.", "Disabled, shadows warmer."] {
            bad(&off, t);
        }
        let id =
            NodeFacts { coverage: 0.0, ..facts([RangeFacts::untouched("shadows"), RangeFacts::untouched("midtones"), RangeFacts::untouched("highlights")]) };
        ok(&id, "No visible change.");
        ok(&id, "It is unchanged.");
        bad(&id, "Slightly warmer.");
        let f = mixed();
        assert!(matches!(check_explanation(&f, "No visible change, the node is disabled."), Err(FactMismatch::NoChange(_))));
        bad(&f, "It does nothing.");
    }

    #[test]
    fn selections_need_the_matching_qualifier_or_window() {
        let f = mixed();
        for t in [
            "It brightens the midtones and warms the skin.",
            "The sky gets warmer.",
            "Inside the window, the midtones get brighter.",
            "Outside the selection, the midtones get brighter.",
            "The window brightens the midtones.",
            "It adds grain to the midtones.",
        ] {
            bad(&f, t);
        }
        let mut q = mixed();
        q.controls.qualifier = Some(QualifierFact {
            hue: 0.08,
            hue_width: 0.1,
            sat: [0.2, 1.0],
            lum: [0.0, 1.0],
            invert: false,
            three_d: false,
            words: "hues around orange (±18°), saturation 20–100 %, luminance 0–100 %".into(),
        });
        q.matte_coverage = Some(0.13);
        ok(&q, "In the selected skin tones, the midtones get brighter and the shadows warmer.");
        ok(&q, "The midtones in the selection get brighter, the shadows warmer.");
        bad(&q, "In the sky, the midtones get brighter and the shadows warmer.");
        bad(&q, "Outside the selection, the midtones get brighter and the shadows warmer.");
        bad(&q, "Inside the window, the midtones get brighter and the shadows warmer.");
        let mut inv = q.clone();
        if let Some(x) = inv.controls.qualifier.as_mut() {
            x.invert = true;
        }
        ok(&inv, "Outside the selection, the midtones get brighter and the shadows warmer.");
        bad(&inv, "Inside the selection, the midtones get brighter and the shadows warmer.");
        bad(&inv, "In the selected skin tones, the midtones get brighter and the shadows warmer.");
        let mut w = mixed();
        w.controls.windows = 1;
        w.matte_coverage = Some(0.5);
        ok(&w, "Inside the window, the midtones get brighter and the shadows warmer.");
        bad(&w, "In the skin tones, the midtones get brighter and the shadows warmer.");
        bad(&w, "The midtones of the whole frame get brighter.");
        ok(&w, "The window brightens the midtones, the shadows get warmer.");
        let mut wi = w.clone();
        wi.controls.windows_inverted = 1;
        bad(&wi, "The window brightens the midtones, the shadows get warmer.");
        ok(&wi, "Outside the window, the midtones get brighter and the shadows warmer.");
        bad(&inv, "The selection brightens the midtones, the shadows get warmer.");
    }

    #[test]
    fn labels_must_be_this_node() {
        let f = mixed();
        let body = "the midtones get half a stop brighter.";
        let lab = |label: Option<&str>, index: Option<usize>, t: &str| check_explanation_labeled(&f, label, index, t);
        assert_eq!(lab(Some("Skin"), Some(2), &format!("Node 2 'Skin': {body}")), Ok(()));
        assert_eq!(lab(Some("Skin"), Some(2), &format!("Node 2 \u{2018}Skin\u{2019}: {body}")), Ok(()));
        assert_eq!(lab(Some("Skin"), Some(2), &format!("Node 2: {body}")), Ok(()));
        for (label, index, t) in [
            (Some("Skin"), Some(2), format!("Node 2 'skin': {body}")),
            (Some("Skin"), Some(2), format!("Node 3 'Skin': {body}")),
            (Some("Skin"), Some(2), format!("Node 2 'Skin brightens': {body}")),
            (None, Some(2), format!("Node 2 'Skin': {body}")),
            (Some("Skin"), None, format!("Node 2: {body}")),
            (None, None, format!("Node 1: {body}")),
            (Some("Skin"), Some(2), format!("Node 2 'Skin': 'the' {body}")),
            (Some("Skin"), Some(2), format!("Node 2 'Skin {body}")),
            (Some("Skin"), Some(2), format!("Node 99999999999999999999: {body}")),
            (Some("Skin"), Some(2), format!("{body} Node 2: {body}")),
        ] {
            assert!(lab(label, index, &t).is_err(), "{t}");
        }
        for t in ["Node", "Node ", "Node 1", "Node 1:", "Node 1 '", "Node 1 ''", "Node 1 '':", "node 1 \u{201c}x", "Nodé 1: x", "Node 1 '\u{2019}':"] {
            let _ = (lab(Some(""), Some(1), t), check_grammar(t), check_vocabulary(t));
        }
    }

    #[test]
    fn spellings() {
        let f = mixed();
        ok(&f, "Mid-tones are more colourful and brighter, the shadows warmer.");
        ok(&f, "Midtones are more colorful and brighter, the shadows warmer.");
    }

    #[test]
    fn vocabulary_comes_from_the_grammar() {
        assert_eq!(GRAMMAR_DOC.matches('"').count() % 2, 0, "balanced quotes");
        let v: &[&str] = &VOCABULARY;
        assert!(v.windows(2).all(|w| w.first() < w.get(1)), "sorted, no duplicates");
        for w in ["shadows", "brighter", "slightly", "untouched", "disabled", "rather", "seventy", "eighths"] {
            assert!(v.contains(&w), "{w}");
        }
        for w in ["darks", "makes", "punch", "skies", "double", "twice", "then", "except", "with"] {
            assert!(!v.contains(&w), "{w}");
        }
        for p in NOOP_PHRASES {
            assert!(p.iter().all(|w| v.contains(w)), "{p:?}");
        }
        assert!(NOOP_DROP.iter().all(|w| v.contains(w)));
    }
}
