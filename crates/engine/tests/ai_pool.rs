//! AI-POOL (fork AI-PLAN, FORK-PLAN §1a.85): the larger question pools in
//! `docs/fork/coach-eval/pool/` (layout in its README.md). Each batch is `<batch>.toml` (held-out
//! test questions: `[[question]]` in the advanced-set format, `[[coach]]` in the keyword-coach
//! format) plus `<batch>.jsonl` (training pairs). The checks: every file is well formed and
//! grounded in real guide sections, no id is used twice, the keyword coach and retrieval stay at
//! their floors, and no test question (or a near-copy of one) is in any training file. The three
//! kinds added by FORK-PLAN §1a.87 (both as pairs and as held-out questions): `task` (spans two or
//! more sections), `chain` (carries 1–3 earlier turns in `context`) and `picture` (carries AI4's
//! measured-fact sheet in `facts`, the wording `PictureFacts::sheet` writes). `creative` items (AI-PLAN
//! AI4 item 5c, FORK-PLAN §1a.91) carry a goal `family` (one creative goal and its variants): a family
//! is all held out or all training, never both.

use filmcraft_engine::Session;
use filmcraft_engine::coach_ask::{self, GUIDE_PAGES};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Keyword-coach pool questions that must pass (measured; may only go up, AI-PLAN AI-POOL).
const POOL_COACH_FLOOR: usize = 142;
/// Advanced pool questions with an expected source in retrieval's top 5 (measured; only up, except
/// 904 → 903 when FEATURE-POOL dropped `feat-pk-trim-gap`, a hit made untrue by package trimming;
/// 911 with feature-project-package-trim; 917 with feature-assistant-search; 920 with
/// feature-project-package-newer; 934 with feature-updates, 944 with feature-gpu-rule, where
/// `feat-pl-igpu-speed` now misses because the XPS guide text on GPUs outranks its section; 948 with
/// feature-nvenc-presets; 956 with feature-nvenc-linux; 960 with feature-nvdec-order; 969 with feature-battery-igpu;
/// 982 with feature-pm-window-icon, where the guide's later text also brings `feat-pl-igpu-speed` back;
/// 980 with S2's remote-location sections: none of them is in the two misses' top 5, the corpus change
/// only flips two near-ties, `pool-e2-through-edits` (10.074 vs 10.115) and `pool-p1-proxy-cache`
/// (15.696 vs 15.724)).
const POOL_RETRIEVAL_FLOOR: usize = 980;
/// Token-set similarity at or above which a training question counts as a near-copy of a test one.
const NEAR_COPY: f64 = 0.7;

const ADV_KINDS: &[&str] = &["node-role", "approach", "tweak", "tool", "multi-step", "pro-practice", "gap", "task", "chain", "picture", "creative"];
const COACH_KINDS: &[&str] = &["order", "what-for", "how", "why", "where", "resolve-word", "premiere-word", "typo", "cut-off"];
const STYLES: &[&str] = &["beginner", "pro", "resolve-words", "premiere-words", "sloppy", "follow-up", "gap"];
const PAIR_KINDS: &[&str] = &["feature", "practice", "mapping", "gap", "follow-up", "task", "chain", "picture", "creative"];
/// Most earlier turns a chain carries.
const MAX_CONTEXT: usize = 3;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolQuestion {
    id: String,
    kind: String,
    q: String,
    sources: Vec<String>,
    must: Vec<String>,
    #[serde(default)]
    must_not: Vec<String>,
    #[serde(default)]
    needs_clip: bool,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    /// A chain's earlier turns, oldest first.
    #[serde(default)]
    context: Vec<Turn>,
    /// A picture case's fact sheet.
    #[serde(default)]
    facts: Option<String>,
    /// A creative question's goal family.
    #[serde(default)]
    family: Option<String>,
}

/// One earlier question and answer of a chain.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    q: String,
    a: String,
}

#[derive(Deserialize)]
struct CoachSection {
    page: String,
    heading: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoachQuestion {
    id: String,
    q: String,
    kind: String,
    origin: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    heading: Option<String>,
    #[serde(default)]
    alt: Vec<CoachSection>,
    status: String,
    #[serde(default)]
    needs_clip: bool,
    facts: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolFile {
    #[serde(default)]
    question: Vec<PoolQuestion>,
    #[serde(default)]
    coach: Vec<CoachQuestion>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolPair {
    q: String,
    a: String,
    sources: Vec<String>,
    kind: String,
    /// For a follow-up: the question it follows.
    #[serde(default)]
    prev: Option<String>,
    #[serde(default)]
    context: Vec<Turn>,
    #[serde(default)]
    facts: Option<String>,
    /// A creative recipe's goal family.
    #[serde(default)]
    family: Option<String>,
}

/// The shape `PictureFacts::sheet` writes: exposure, white balance, saturation and skin lines.
fn is_fact_sheet(f: &str) -> bool {
    let l: Vec<&str> = f.lines().collect();
    l.len() == 4
        && l[0].starts_with("Exposure: mids about ")
        && l[0].ends_with("stops (5–95 %).")
        && l[1].starts_with("White balance: ")
        && ["Saturation: low (muted).", "Saturation: moderate.", "Saturation: high."].contains(&l[2])
        && l[3].starts_with("Skin-tone colours")
}

/// The new kinds' shape: a chain (only) has earlier turns, a picture case (only) a fact sheet and
/// needs the clip, a task names two or more sections.
fn check_kind_shape(at: &str, kind: &str, sources: usize, context: &[Turn], facts: Option<&str>, needs_clip: Option<bool>) {
    assert!((kind == "chain") == !context.is_empty(), "{at}: a chain (and only a chain) carries earlier turns in `context`");
    assert!(context.len() <= MAX_CONTEXT, "{at}: at most {MAX_CONTEXT} earlier turns");
    assert!(context.iter().all(|t| !t.q.trim().is_empty() && !t.a.trim().is_empty()), "{at}: empty earlier turn");
    assert!((kind == "picture") == facts.is_some(), "{at}: a picture case (and only one) carries its fact sheet in `facts`");
    if let Some(f) = facts {
        assert!(is_fact_sheet(f), "{at}: `facts` is not in the fact-sheet wording:\n{f}");
        assert!(needs_clip.is_none_or(|n| n), "{at}: a picture question needs_clip");
    }
    assert!(kind != "task" || sources >= 2, "{at}: a task spans two or more sections");
    assert!(kind != "creative" || sources >= 2, "{at}: a creative plan uses two or more sections");
}

/// A creative item (and only one) names its goal family: lower-case words joined by `-`.
fn check_family(at: &str, kind: &str, family: Option<&str>) {
    assert!((kind == "creative") == family.is_some(), "{at}: a creative item (and only one) names its `family`");
    if let Some(f) = family {
        let ok = !f.is_empty() && f.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !f.starts_with('-') && !f.ends_with('-');
        assert!(ok, "{at}: family {f:?} is not lower-case words joined by `-`");
    }
}

#[test]
fn the_family_check_accepts_only_creative_items_with_a_family() {
    check_family("ok", "creative", Some("ghost-figure"));
    check_family("ok", "task", None);
    for (kind, fam) in [("creative", None), ("task", Some("ghost")), ("creative", Some("Ghost")), ("creative", Some("")), ("creative", Some("-x"))] {
        assert!(std::panic::catch_unwind(|| check_family("bad", kind, fam)).is_err(), "{kind} {fam:?} should fail");
    }
}

#[test]
fn the_fact_sheet_check_matches_what_the_engine_writes() {
    for c in [[0.18_f32; 3], [0.05, 0.04, 0.025], [0.15, 0.17, 0.22], [0.9, 0.6, 0.45]] {
        let img = filmcraft_render::Image::filled(64, 36, [c[0], c[1], c[2], 1.0]);
        let f = filmcraft_render::picture_facts::PictureFacts::measure(&img, filmcraft_color::WorkingSpace::Rec709).expect("facts");
        assert!(is_fact_sheet(&f.sheet()), "{}", f.sheet());
    }
    assert!(!is_fact_sheet("Exposure: fine.\nWhite balance: warm."));
}

#[derive(Deserialize)]
struct IdsOnly {
    question: Vec<IdOnly>,
}

#[derive(Deserialize)]
struct IdOnly {
    id: String,
    q: String,
}

fn eval_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/fork/coach-eval")
}

/// The pool's files with the given extension, sorted (the daily sample is not a batch).
fn pool_files(ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(eval_dir().join("pool"))
        .expect("the pool folder exists")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == ext) && p.file_stem().is_some_and(|s| s != "daily-sample"))
        .collect();
    v.sort();
    v
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn batches() -> Vec<(String, PoolFile)> {
    pool_files("toml").iter().map(|p| (p.display().to_string(), toml::from_str(&read(p)).unwrap_or_else(|e| panic!("{}: {e}", p.display())))).collect()
}

fn pairs() -> Vec<(String, PoolPair)> {
    let mut out = Vec::new();
    for p in pool_files("jsonl") {
        for (i, line) in read(&p).lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let pair = serde_json::from_str(line).unwrap_or_else(|e| panic!("{} line {}: {e}", p.display(), i + 1));
            out.push((format!("{} line {}", p.display(), i + 1), pair));
        }
    }
    out
}

fn header_ids() -> HashSet<String> {
    GUIDE_PAGES
        .iter()
        .filter(|(p, _)| *p != "README.md")
        .flat_map(|(p, src)| coach_ask::split_page(p, src))
        .filter_map(|s| s.header.as_deref().and_then(|h| coach_ask::parse_header(h).ok()).map(|h| h.id))
        .collect()
}

/// Every `page › heading` of the guide.
fn headings() -> HashSet<String> {
    GUIDE_PAGES.iter().flat_map(|(p, src)| coach_ask::split_page(p, src)).map(|s| format!("{} › {}", s.page, s.heading)).collect()
}

/// Backticked command-like ids the guide names (as `coach_ask_tests::guide_command_ids`).
fn guide_command_ids() -> HashSet<String> {
    GUIDE_PAGES
        .iter()
        .flat_map(|(_, src)| src.split('`'))
        .filter(|c| c.contains('.') && c.bytes().next().is_some_and(|b| b.is_ascii_lowercase()) && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'))
        .map(str::to_string)
        .collect()
}

/// Question words that carry no topic: dropped before comparing two questions, so "how do I track
/// a window" and "how do I feather a window" don't count as copies of each other.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "can", "d", "do", "does", "how", "i", "if", "in", "is", "it", "my", "of", "on", "or", "s", "should", "that", "the", "this", "to",
    "what", "when", "where", "which", "why", "with",
];

/// The question's lower-case words without [`STOPWORDS`] (all of them when nothing else is left).
fn tokens(s: &str) -> Vec<String> {
    let all: Vec<String> = s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    let content: Vec<String> = all.iter().filter(|w| !STOPWORDS.contains(&w.as_str())).cloned().collect();
    if content.is_empty() { all } else { content }
}

/// Jaccard similarity of the two questions' word sets (1.0 for the same words in any order).
fn similarity(a: &[String], b: &[String]) -> f64 {
    let a: HashSet<&String> = a.iter().collect();
    let b: HashSet<&String> = b.iter().collect();
    let union = a.union(&b).count();
    if union == 0 { 1.0 } else { a.intersection(&b).count() as f64 / union as f64 }
}

#[test]
fn pool_files_are_well_formed_and_grounded() {
    let ids = header_ids();
    let heads = headings();
    let named = guide_command_ids();
    let s = Session::default();
    let base_adv: IdsOnly = toml::from_str(&read(&eval_dir().join("advanced.toml"))).expect("advanced.toml");
    let base_coach: IdsOnly = toml::from_str(&read(&eval_dir().join("questions.toml"))).expect("questions.toml");
    let mut seen: HashSet<String> = base_adv.question.iter().chain(&base_coach.question).map(|q| q.id.clone()).collect();
    for (file, b) in batches() {
        for q in &b.question {
            assert!(seen.insert(q.id.clone()), "{file}: {}: id used twice", q.id);
            assert!(ADV_KINDS.contains(&q.kind.as_str()), "{file}: {}: kind {}", q.id, q.kind);
            assert!(!q.q.trim().is_empty() && !q.must.is_empty() && !q.sources.is_empty(), "{file}: {}: q, must, sources required", q.id);
            assert!(q.sources.iter().all(|x| ids.contains(x)), "{file}: {}: unknown source in {:?}", q.id, q.sources);
            assert!(q.must.iter().chain(&q.must_not).all(|f| !f.trim().is_empty()), "{file}: {}: empty fact", q.id);
            assert!(q.style.as_deref().is_none_or(|x| STYLES.contains(&x)), "{file}: {}: style {:?}", q.id, q.style);
            assert!(q.origin.as_deref().is_none_or(|x| ["agent", "owner-verbatim", "owner-paraphrase"].contains(&x)), "{file}: {}: origin", q.id);
            check_kind_shape(&format!("{file}: {}", q.id), &q.kind, q.sources.len(), &q.context, q.facts.as_deref(), Some(q.needs_clip));
            check_family(&format!("{file}: {}", q.id), &q.kind, q.family.as_deref());
        }
        for q in &b.coach {
            assert!(seen.insert(q.id.clone()), "{file}: {}: id used twice", q.id);
            assert!(COACH_KINDS.contains(&q.kind.as_str()), "{file}: {}: kind {}", q.id, q.kind);
            assert!(["covered", "partial", "uncovered"].contains(&q.status.as_str()), "{file}: {}: status {}", q.id, q.status);
            assert!(["agent", "owner-verbatim", "owner-paraphrase"].contains(&q.origin.as_str()), "{file}: {}: origin", q.id);
            assert!(!q.q.trim().is_empty() && !q.facts.is_empty(), "{file}: {}: q and facts required", q.id);
            if q.status == "uncovered" {
                continue;
            }
            let (Some(p), Some(h)) = (&q.page, &q.heading) else { panic!("{file}: {}: page and heading required", q.id) };
            for want in std::iter::once(format!("{p} › {h}")).chain(q.alt.iter().map(|x| format!("{} › {}", x.page, x.heading))) {
                assert!(heads.contains(&want), "{file}: {}: no section {want}", q.id);
            }
            let _ = (&q.context, &q.note, q.needs_clip);
        }
    }
    let mut n = 0;
    for (at, p) in pairs() {
        assert!(!p.q.trim().is_empty() && !p.a.trim().is_empty(), "{at}: empty q / a");
        assert!(PAIR_KINDS.contains(&p.kind.as_str()), "{at}: kind {}", p.kind);
        assert!((p.kind == "follow-up") == p.prev.is_some(), "{at}: a follow-up (and only one) names its `prev` question");
        check_kind_shape(&at, &p.kind, p.sources.len(), &p.context, p.facts.as_deref(), None);
        check_family(&at, &p.kind, p.family.as_deref());
        assert!(p.context.iter().all(|t| !t.a.contains("{ui:")), "{at}: raw ui id in an earlier turn");
        assert!(!p.sources.is_empty() && p.sources.iter().all(|x| ids.contains(x)), "{at}: unknown source in {:?}", p.sources);
        assert!(!p.a.contains("{ui:"), "{at}: raw ui id");
        for chunk in p.a.split('`').skip(1).step_by(2) {
            let is_cmd = chunk.contains('.')
                && chunk.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && chunk.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.');
            if is_cmd && !chunk.contains("..") {
                assert!(s.shortcuts.is_known_command(chunk) || named.contains(chunk), "{at}: `{chunk}` is not a known command");
            }
        }
        n += 1;
    }
    eprintln!("AI-POOL: {} batches, {n} training pairs", pool_files("toml").len());
}

/// A creative goal family is all held out or all training (AI-PLAN AI4 item 5c): no family named by
/// a held-out `.toml` question is named by a training pair.
#[test]
fn no_creative_family_is_both_held_out_and_training() {
    let held: HashSet<String> = batches().into_iter().flat_map(|(_, b)| b.question).filter_map(|q| q.family).collect();
    let both: Vec<String> = pairs().into_iter().filter_map(|(at, p)| p.family.filter(|f| held.contains(f)).map(|f| format!("{at}: {f}"))).collect();
    assert!(both.is_empty(), "creative families both held out and in training:\n{}", both.join("\n"));
    let trained: HashSet<String> = pairs().into_iter().filter_map(|(_, p)| p.family).collect();
    eprintln!("creative families: {} held out, {} training", held.len(), trained.len());
}

/// Held-out means held out: no test question (advanced.toml, questions.toml, the pool's batches)
/// is in a training file (training-pairs.jsonl, the pool's .jsonl) word for word or nearly.
#[test]
fn no_test_question_is_in_the_training_pairs() {
    let mut tests: Vec<(String, String)> = Vec::new();
    for f in ["advanced.toml", "questions.toml"] {
        let b: IdsOnly = toml::from_str(&read(&eval_dir().join(f))).expect("eval file parses");
        tests.extend(b.question.into_iter().map(|q| (q.id, q.q)));
    }
    for (_, b) in batches() {
        // a chain's opening question is held out as well as its last one
        tests.extend(b.question.iter().filter_map(|q| q.context.first().map(|t| (format!("{} (first turn)", q.id), t.q.clone()))));
        tests.extend(b.question.into_iter().map(|q| (q.id, q.q)));
        tests.extend(b.coach.into_iter().map(|q| (q.id, q.q)));
    }
    let mut train: Vec<(String, String)> = Vec::new();
    for (i, line) in read(&eval_dir().join("training-pairs.jsonl")).lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).expect("training pair parses");
        train.push((format!("training-pairs.jsonl line {}", i + 1), v["q"].as_str().unwrap_or_default().to_string()));
    }
    for (at, p) in pairs() {
        if let Some(t) = p.context.first() {
            train.push((format!("{at} (first turn)"), t.q.clone()));
        }
        train.push((at, p.q));
    }
    let train: Vec<(String, String, Vec<String>)> = train
        .into_iter()
        .map(|(at, q)| {
            let t = tokens(&q);
            (at, q, t)
        })
        .collect();
    let mut hits = Vec::new();
    for (id, q) in &tests {
        let t = tokens(q);
        for (at, tq, tt) in &train {
            let sim = similarity(&t, tt);
            if sim >= NEAR_COPY {
                hits.push(format!("{id} {q:?} ~ {at} {tq:?} ({sim:.2})"));
            }
        }
    }
    assert!(hits.is_empty(), "test questions in the training data:\n{}", hits.join("\n"));
    // the check itself: a reordered copy is caught, a different question isn't
    assert!(similarity(&tokens("How do I add a node?"), &tokens("add a node: how do I")) >= NEAR_COPY);
    assert!(similarity(&tokens("what is gain"), &tokens("what is gamma")) < NEAR_COPY);
    assert!(similarity(&tokens("how do I track a window"), &tokens("How do I feather a window?")) < NEAR_COPY);
}

/// The keyword coach answers the pool's `[[coach]]` questions as `questions.toml`'s are scored
/// (covered / partial: the best section is `page › heading` or an `alt`; uncovered: not covered).
#[test]
fn pool_coach_questions_pass_at_least_the_floor() {
    let s = Session::default();
    let (mut pass, mut total, mut misses) = (0, 0, Vec::new());
    for (_, b) in batches() {
        for q in &b.coach {
            total += 1;
            let a = s.coach_ask(&q.q);
            let best = a.sources.first().map(|x| format!("{} › {}", x.page, x.heading));
            let ok = if q.status == "uncovered" {
                !a.covered && a.steps.is_empty() && a.sources.is_empty()
            } else {
                let want: Vec<String> = q
                    .page
                    .iter()
                    .zip(&q.heading)
                    .map(|(p, h)| format!("{p} › {h}"))
                    .chain(q.alt.iter().map(|x| format!("{} › {}", x.page, x.heading)))
                    .collect();
                a.covered && best.as_ref().is_some_and(|b| want.contains(b))
            };
            if ok {
                pass += 1;
            } else {
                misses.push(format!("{} {:?}: covered {} best {}", q.id, q.q, a.covered, best.as_deref().unwrap_or("—")));
            }
        }
    }
    eprintln!("AI-POOL coach: {pass} / {total} (floor {POOL_COACH_FLOOR}); misses:\n{}", misses.join("\n"));
    assert!(pass >= POOL_COACH_FLOOR, "AI-POOL coach: {pass} / {total} below the floor {POOL_COACH_FLOOR}");
}

/// Retrieval (what the assistant's model reads) finds an expected source of the pool's advanced
/// questions in its top 5 at least as often as the floor.
#[test]
fn pool_retrieval_finds_the_sources() {
    let (mut hits, mut firsts, mut total, mut misses) = (0, 0, 0, Vec::new());
    for (_, b) in batches() {
        // creative plans are scored apart (`creative_retrieval_covers_the_plan`)
        for q in b.question.iter().filter(|q| q.kind != "creative") {
            total += 1;
            // a chain's last question leans on the one before it, as the Assistant's history does
            let asked = q.context.last().map_or_else(|| q.q.clone(), |t| format!("{} {}", t.q, q.q));
            let got = coach_ask::retrieve(&asked, 5);
            firsts += usize::from(got.first().and_then(|r| r.header).is_some_and(|h| q.sources.contains(&h.id)));
            if got.iter().any(|r| r.header.is_some_and(|h| q.sources.contains(&h.id))) {
                hits += 1;
            } else {
                misses.push(q.id.as_str().to_string());
            }
        }
    }
    eprintln!("AI-POOL retrieval recall@5: {hits} / {total}, recall@1: {firsts}; misses: {misses:?}");
    assert!(hits >= POOL_RETRIEVAL_FLOOR, "AI-POOL recall@5 fell to {hits} / {total} (floor {POOL_RETRIEVAL_FLOOR})");
}

/// `pool/daily-sample.toml` (chosen by `tools/fork/ai-pool-daily-sample.py`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DailySample {
    owner: Vec<String>,
    ids: Vec<String>,
}

#[derive(Deserialize)]
struct OriginsOnly {
    question: Vec<WithOrigin>,
}

#[derive(Deserialize)]
struct WithOrigin {
    id: String,
    #[serde(default)]
    origin: Option<String>,
}

/// Creative questions' plan sections among the sections the assistant's model is sent
/// (`retrieve_with_picture`, `SECTIONS`), summed over the batch (measured 2026-10-15: 6 / 21 on the
/// bare question, 8 / 21 with per-sub-goal retrieval, AI-PLAN AI4 item 5c.2; 9 / 22 with the
/// `effects.opacity` guide section as a ghost source; 11 / 24 with the shake and clean-plate
/// practice notes; 56 / 101 with CREATIVE-POOL's held-out batch creative-2; 92 / 182 with creative-3; 128 / 251 with creative-4; 160 / 335 with creative-5; 162 / 339 with the `effects.stylize` section as a source; 197 / 413 with creative-6;
/// 235 / 493 with creative-7; 272 / 570 with creative-8; 301 / 646
/// with creative-9; 326 / 723 with creative-10; 346 / 806 with
/// creative-11; 354 / 806 after the main runner's building-block wording;
/// 374 / 883 with creative-14; 369 / 883 once the effect-reference guide page joined the
/// sections retrieval competes over (lowered on purpose, FORK-PLAN §7 2026-10-15 23:55); 371 / 887
/// with the effect-reference transform section as a source; 381 / 958 with creative-16; 380 / 958
/// after the VHS note's rewording (50407a37, lowered on purpose); 379 / 958 after the timelapse and
/// overlay practice notes (b87ca4df, lowered on purpose); 376 / 958 after the effects.md sections
/// lens-light / shape-move / painterly (dad97cc6, lowered on purpose; written 6 by mistake there);
/// 408 / 1031 with creative-18 and the creative plans citing those sections instead of the
/// effect reference (CREATIVE-POOL pool-21); 420 / 1109 with creative-22; 439 / 1181 with creative-24;
/// 462 / 1250 with creative-26; 476 / 1313 with creative-28; 484 / 1374 with creative-30; 492 / 1423 with creative-32; 510 / 1472 with creative-34; 537 / 1517 with creative-36; 555 / 1550 with creative-38;
/// only up otherwise).
/// 556 after the guide's Motion (horizon / TV squeeze) and Dip-at-the-ends lines (main runner, 2026-10-16);
/// 557 with the card-like transitions (Page Peel, 3D Spin, …) named in `effects.transitions`;
/// 556 after the new `practice.titles.write-on` section (no plan cites it yet; it takes one slot);
/// 558 measured 2026-10-16 (FEATURE-POOL; no creative items added, the guide's later sections).
/// 559 measured 2026-10-16 (FEATURE-POOL package trim; no creative items added, the guide's new sections).
/// 560 measured 2026-10-20 (FEATURE-POOL pm-window-icon; no creative items added, the guide's §1a.108 text).
/// 559 measured 2026-10-20 (S2's remote-location sections; no creative items added, corpus statistics only).
const CREATIVE_COVERAGE_FLOOR: usize = 559;

/// AI-PLAN AI4 item 5c: a creative question needs a section for each sub-task, not one best match,
/// so it is scored by how many of its plan's sections the model is sent (coverage), apart from the
/// lookup questions' recall@5; per-sub-goal retrieval must not cost the lookup questions more than
/// 1 % of their hits.
#[test]
fn creative_retrieval_covers_the_plan() {
    use filmcraft_engine::coach_assist::{SECTIONS, retrieve_with_picture};
    let covers = |got: &[coach_ask::Retrieved], srcs: &[String]| srcs.iter().filter(|s| got.iter().any(|r| r.header.is_some_and(|h| &h.id == *s))).count();
    let all: Vec<_> = batches().into_iter().flat_map(|(_, b)| b.question).collect();
    let (mut bare, mut found, mut wanted, mut short) = (0, 0, 0, Vec::new());
    for q in all.iter().filter(|q| q.kind == "creative") {
        bare += covers(&coach_ask::retrieve(&q.q, SECTIONS), &q.sources);
        let n = covers(&retrieve_with_picture(&q.q, &q.q, None, SECTIONS), &q.sources);
        found += n;
        wanted += q.sources.len();
        if n < q.sources.len() {
            short.push(format!("{} {n}/{}", q.id, q.sources.len()));
        }
    }
    eprintln!("creative coverage@{SECTIONS}: {found} / {wanted} (bare question {bare}); short: {short:?}");
    assert!(found >= CREATIVE_COVERAGE_FLOOR, "creative coverage fell to {found} / {wanted} (floor {CREATIVE_COVERAGE_FLOOR})");
    let lookup: Vec<_> = all.iter().filter(|q| q.kind != "creative" && q.context.is_empty() && q.facts.is_none()).collect();
    let hit = |got: Vec<coach_ask::Retrieved>, srcs: &[String]| usize::from(covers(&got, srcs) > 0);
    let before: usize = lookup.iter().map(|q| hit(coach_ask::retrieve(&q.q, SECTIONS), &q.sources)).sum();
    let after: usize = lookup.iter().map(|q| hit(retrieve_with_picture(&q.q, &q.q, None, SECTIONS), &q.sources)).sum();
    eprintln!("lookup hits@{SECTIONS}: {before} -> {after} with sub-goals");
    assert!(after.saturating_mul(100) >= before.saturating_mul(99), "sub-goals cost the lookup questions too much: {before} -> {after}");
}

/// Sub-goal splitting is bounded and leaves single-goal questions alone, whatever the input.
#[test]
fn sub_goals_are_bounded() {
    use filmcraft_engine::coach_assist::sub_goals;
    assert!(sub_goals("how do I add film grain").is_empty());
    assert_eq!(sub_goals("freeze the frame and make the colour drain out of it"), ["freeze the frame", "make the colour drain out of it"]);
    let hostile = "mask the person and ".repeat(10_000);
    assert!(sub_goals(&hostile).len() <= 6);
    for q in ["", "and and and", ",,,;;;", "\u{0}\u{202e} and \u{feff}", &"é".repeat(100_000)] {
        assert!(sub_goals(q).len() <= 6);
    }
}

/// The daily sample (AI-PLAN AI-POOL): about 150 questions, every id a real held-out pool question
/// (no repeats), every batch represented, and the owner's verbatim questions always in.
#[test]
fn the_daily_sample_is_real_spread_and_keeps_the_owners_questions() {
    let d: DailySample = toml::from_str(&read(&eval_dir().join("pool/daily-sample.toml"))).expect("daily-sample.toml");
    let mut batch_of = std::collections::HashMap::new();
    for (file, b) in batches() {
        for q in &b.question {
            batch_of.insert(q.id.clone(), file.clone());
        }
    }
    let mut seen = HashSet::new();
    for i in &d.ids {
        assert!(seen.insert(i), "{i} is in the daily sample twice");
        assert!(batch_of.contains_key(i), "{i} is not a held-out pool question");
    }
    let n = d.ids.len() + d.owner.len();
    assert!((140..=160).contains(&n), "the daily sample has {n} questions, want about 150");
    let covered: HashSet<&String> = d.ids.iter().filter_map(|i| batch_of.get(i)).collect();
    for (file, b) in batches() {
        assert!(b.question.is_empty() || covered.contains(&file), "{file} has no question in the daily sample");
    }
    let base: OriginsOnly = toml::from_str(&read(&eval_dir().join("questions.toml"))).expect("questions.toml");
    for q in base.question.iter().filter(|q| q.origin.as_deref() == Some("owner-verbatim")) {
        assert!(d.owner.contains(&q.id), "owner-verbatim {} is not in the daily sample", q.id);
    }
    assert!(d.owner.iter().all(|o| base.question.iter().any(|q| &q.id == o)), "an `owner` id is not in questions.toml");
}

/// Picture questions with an expected source in the top 5 when retrieval also reads the frame's
/// fact sheet (measured 2026-10-15: 8 / 10; 3 on the bare question; only up).
const PICTURE_RETRIEVAL_FLOOR: usize = 8;

/// AI4.5a: the assistant's retrieval reads the measured fact sheet for a question about the frame
/// (`coach_assist::retrieve_with_picture`), and a sheet that has nothing to do with a question
/// costs the other pool questions little (measured 766–771 of 781 with each picture case's sheet).
#[test]
fn picture_questions_retrieve_with_the_fact_sheet() {
    use filmcraft_engine::coach_assist::retrieve_with_picture;
    let hit =
        |q: &str, sheet: Option<&str>, srcs: &[String]| retrieve_with_picture(q, q, sheet, 5).iter().any(|r| r.header.is_some_and(|h| srcs.contains(&h.id)));
    let all: Vec<_> = batches().into_iter().flat_map(|(_, b)| b.question).collect();
    let pictures: Vec<_> = all.iter().filter(|q| q.facts.is_some()).collect();
    let found = pictures.iter().filter(|q| hit(&q.q, q.facts.as_deref(), &q.sources)).count();
    eprintln!("picture retrieval@5 with the sheet: {found} / {}", pictures.len());
    assert!(found >= PICTURE_RETRIEVAL_FLOOR, "picture retrieval fell to {found} (floor {PICTURE_RETRIEVAL_FLOOR})");
    let others: Vec<_> = all.iter().filter(|q| q.facts.is_none() && q.context.is_empty()).collect();
    let bare = others.iter().filter(|q| hit(&q.q, None, &q.sources)).count();
    for sheet in pictures.iter().filter_map(|q| q.facts.as_deref()) {
        let with = others.iter().filter(|q| hit(&q.q, Some(sheet), &q.sources)).count();
        // at most 3 % lost to an unrelated sheet
        assert!(with.saturating_mul(100) >= bare.saturating_mul(97), "an unrelated sheet costs too much: {bare} -> {with}\n{sheet}");
    }
}

/// Fork §1a.98 (Q45 (a)): search by meaning is kept only if it raises creative plan coverage
/// (at [`SECTIONS`], with picture / technique steering as the assistant uses it) without costing
/// the lookup questions' hits. Prints words / meaning / mixed side by side; needs the embedding
/// model in `<data dir>/models/embed/bge-small-en-v1.5` (owner question Q51) and minutes of CPU
/// the first time (the section vectors are cached next to the model after that).
/// `FILMCRAFT_DATA_DIR=~/.local/share/filmcraft-fork fc-cargo.sh test -p filmcraft-engine --release --features llm --test ai_pool search_by_meaning -- --ignored --nocapture`
#[cfg(feature = "llm")]
#[test]
#[ignore = "needs the embedding model (FILMCRAFT_DATA_DIR) and minutes of CPU the first time"]
fn search_by_meaning_coverage() {
    use filmcraft_engine::coach_assist::{SECTIONS, retrieve_with_picture_in};
    use filmcraft_engine::coach_meaning::{SearchMode, available};
    assert!(available(), "no embedding model in <data dir>/models/embed/bge-small-en-v1.5 (FILMCRAFT_DATA_DIR?)");
    let covers = |got: &[coach_ask::Retrieved], srcs: &[String]| srcs.iter().filter(|s| got.iter().any(|r| r.header.is_some_and(|h| &h.id == *s))).count();
    let all: Vec<_> = batches().into_iter().flat_map(|(_, b)| b.question).collect();
    let creative: Vec<_> = all.iter().filter(|q| q.kind == "creative").collect();
    let lookup: Vec<_> = all.iter().filter(|q| q.kind != "creative" && q.context.is_empty() && q.facts.is_none()).collect();
    let wanted: usize = creative.iter().map(|q| q.sources.len()).sum();
    let mut words = (0, 0, 0);
    for mode in [SearchMode::Words, SearchMode::Meaning, SearchMode::Mixed] {
        let mut found = 0;
        let mut planned = 0; // questions with ≥ 2 plan sections in the top SECTIONS (§1a.98's measure)
        for q in &creative {
            let n = covers(&retrieve_with_picture_in(mode, &q.q, &q.q, None, SECTIONS), &q.sources);
            found += n;
            planned += usize::from(n >= 2.min(q.sources.len()));
        }
        let hits: usize = lookup.iter().map(|q| usize::from(covers(&retrieve_with_picture_in(mode, &q.q, &q.q, None, SECTIONS), &q.sources) > 0)).sum();
        eprintln!(
            "SEARCH {mode:?}: creative coverage@{SECTIONS} {found} / {wanted}, questions with >= 2 plan sections {planned} / {}, lookup hits {hits} / {}",
            creative.len(),
            lookup.len()
        );
        if mode == SearchMode::Words {
            words = (found, planned, hits);
        } else {
            eprintln!(
                "  vs words: coverage {:+}, planned {:+}, lookup {:+}",
                found as i64 - words.0 as i64,
                planned as i64 - words.1 as i64,
                hits as i64 - words.2 as i64
            );
        }
    }
}
