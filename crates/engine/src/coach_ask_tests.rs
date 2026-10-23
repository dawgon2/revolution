//! "Ask the coach" (fork GRADE-PLAN G2.12, part 2): the guide index, the 30-question set, the
//! command check and hostile questions.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Session;
use crate::coach_ask::{self, Checks, GUIDE_PAGES, MAX_ANSWER_LINES, MAX_RETRIEVE, ground, ground_with_text, retrieve, stem, tokenize};

#[derive(Deserialize)]
struct QuestionFile {
    question: Vec<Question>,
    offtopic: Vec<OffTopic>,
}

#[derive(Deserialize)]
struct Question {
    q: String,
    page: String,
    heading: String,
}

#[derive(Deserialize)]
struct OffTopic {
    ask: String,
}

fn question_file() -> QuestionFile {
    toml::from_str(include_str!("../../../assets/coach/questions.toml")).expect("questions.toml parses")
}

fn questions() -> Vec<Question> {
    question_file().question
}

/// The session's checks without a UI, plus an automation-id check that accepts everything.
fn with_ui(q: &str) -> coach_ask::Answer {
    let s = Session::default();
    let command = |id: &str| s.shortcuts.is_known_command(id);
    let namespace = |ns: &str| s.shortcuts.is_command_namespace(ns);
    let ui = |_: &str| true;
    coach_ask::ask(q, &Checks { command: &command, namespace: &namespace, ui: Some(&ui) })
}

fn ask(s: &mut Session, q: &str) -> Value {
    s.execute("coach.ask", json!({ "question": q })).expect("coach.ask answers")
}

#[test]
fn the_page_list_matches_the_guide_directory() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/guide");
    let mut on_disk = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).expect("docs/guide is readable") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "md") {
                on_disk.push(p.strip_prefix(&root).expect("under root").to_string_lossy().replace('\\', "/"));
            }
        }
    }
    on_disk.sort();
    let mut listed: Vec<String> = GUIDE_PAGES.iter().map(|(p, _)| p.to_string()).collect();
    listed.sort();
    assert_eq!(listed, on_disk, "add new guide pages to coach_ask::GUIDE_PAGES");
}

#[test]
fn every_indexed_section_has_provenance() {
    assert!(coach_ask::sections().len() > 50);
    for s in coach_ask::sections() {
        assert!(!s.provenance.is_empty(), "{} › {} has no provenance tag", s.page, s.heading);
    }
    assert!(!coach_ask::sections().iter().any(|s| s.page == "README.md"), "the guide's own README is not searched");
}

#[test]
fn tokenizing_normalises_stems_and_drops_stop_words() {
    assert_eq!(tokenize("How do I make the Colour grey?", 100), ["color", "gray"]);
    assert_eq!(stem("grading"), stem("graded"));
    assert_eq!(stem("grade"), stem("grades"));
    assert_eq!(stem("darker"), "dark");
    assert_eq!(stem("matches"), "match");
    assert_eq!(stem("cutting"), "cut");
    assert_eq!(stem("qualifiers"), stem("qualifier"));
    // non-ASCII words are kept whole
    assert_eq!(tokenize("Ñandú 天空", 100), ["ñandú", "天空"]);
}

#[test]
fn steps_come_from_the_numbered_list_with_their_ids() {
    let sec = coach_ask::sections().iter().find(|s| s.heading.starts_with("3. Make the sky bluer")).expect("the sky recipe");
    let steps = coach_ask::section_steps(&sec.body);
    assert_eq!(steps.len(), 7);
    assert_eq!(steps[1].ui, ["grade.tab.qualifier", "grade.qual.pick"]);
    assert_eq!(steps[1].commands, ["grade.pickQualifier"]);
    assert!(steps[1].text.starts_with("Open the Qualifier tab and click Pick, then click the sky"), "{}", steps[1].text);
    assert!(!steps.iter().any(|s| s.text.contains("**") || s.text.contains("{ui:")));
    assert_eq!(sec.provenance, ["[Resolve-style]"]);
}

#[test]
fn thirty_beginner_questions_find_their_section_and_pass_the_command_check() {
    let mut s = Session::default();
    let qs = questions();
    assert_eq!(qs.len(), 30);
    let mut hits = 0;
    let mut misses = Vec::new();
    for q in &qs {
        let best = coach_ask::rank(&q.q).first().and_then(|(i, _)| coach_ask::sections().get(*i)).map(|s| (s.page, s.heading.as_str()));
        if best == Some((q.page.as_str(), q.heading.as_str())) {
            hits += 1;
        } else {
            misses.push(format!("{:?} → {best:?} (want {} › {})", q.q, q.page, q.heading));
        }
        let a = ask(&mut s, &q.q);
        assert_eq!(a["covered"], json!(true), "{:?} is covered by the guide: {a}", q.q);
        for st in a["steps"].as_array().expect("steps") {
            for c in st["command"].as_str().into_iter().chain(st["commands"].as_array().into_iter().flatten().filter_map(Value::as_str)) {
                assert!(s.shortcuts.is_known_command(c), "{:?}: unknown command {c}", q.q);
            }
            assert!(st["ui"].is_null() && st["uis"].is_null(), "{:?}: no UI, so no automation ids: {st}", q.q);
            let text = st["text"].as_str().unwrap_or("");
            assert!(!text.is_empty() && !text.contains("{ui:") && !text.contains(",,") && !text.contains("()"), "{:?}: {text:?}", q.q);
        }
        assert!(!a["summary"].as_str().unwrap_or("").contains("{ui:"));
    }
    eprintln!("ask the coach: {hits}/30 best sections; misses: {misses:#?}");
    // 28 until C3.11; 29 with BM25 b 0.6 and `compositor-trackers.md` indexed (27 on the merged
    // C3.10 corpus at b 0.5 even without that page)
    assert!(hits >= 29, "only {hits}/30 questions found their section first: {misses:#?}");
}

#[test]
fn questions_the_guide_does_not_cover_fall_back() {
    let mut s = Session::default();
    let off = question_file().offtopic;
    assert!(off.len() >= 8);
    // (word matching, like the 28 / 30 of the covered questions: one off-topic question may land on a
    // section that happens to hold all its words, e.g. "add a title with my name" on Primaries ▸ The
    // panel ("Add Node", a wheel's title, "names the node") since the audio page shifted the weights;
    // a titles page in the guide is the real fix)
    let mut misses = Vec::new();
    for q in &off {
        let a = ask(&mut s, &q.ask);
        if a["covered"] == json!(true) {
            misses.push(format!("{:?}: {}", q.ask, a["sources"]));
            continue;
        }
        assert_eq!((a["covered"].clone(), a["fallback"].clone()), (json!(false), json!(true)), "{:?}: {a}", q.ask);
        assert_eq!(a["steps"], json!([]), "{:?}", q.ask);
        assert_eq!(a["sources"], json!([]), "{:?}", q.ask);
        assert!(a["message"].as_str().is_some_and(|m| m.starts_with("The guide doesn't cover this yet")), "{:?}: {a}", q.ask);
        assert!(a["lesson"].is_null(), "{:?}: no lesson is about this: {a}", q.ask);
    }
    eprintln!("off-topic questions answered anyway: {misses:#?}");
    assert!(misses.len() <= 1, "off-topic questions answered as covered: {misses:#?}");
}

#[test]
fn unknown_ids_drop_their_step_and_are_counted() {
    let ok = |_: &str| true;
    let none = |_: &str| false;
    let not_pick = |id: &str| id != "grade.qual.pick";
    let q = "how do I make the sky bluer without changing skin?";
    let all = coach_ask::ask(q, &Checks { command: &ok, namespace: &ok, ui: Some(&ok) });
    assert_eq!((all.steps.len(), all.removed), (7, 0));
    assert_eq!(all.steps[1].ui, ["grade.tab.qualifier", "grade.qual.pick"]);
    let a = coach_ask::ask(q, &Checks { command: &ok, namespace: &ok, ui: Some(&not_pick) });
    assert_eq!((a.steps.len(), a.removed, a.removed_controls, a.removed_commands), (6, 1, 1, 0));
    assert_eq!(a.removed_ids, ["grade.qual.pick"]);
    // no command known: the steps naming one go; with every step gone, the lesson is offered
    let b = coach_ask::ask(q, &Checks { command: &none, namespace: &ok, ui: Some(&none) });
    assert!(b.steps.is_empty() && b.fallback && b.removed == 7);
    assert!(b.removed_controls > 0 && b.removed_commands > 0);
    assert_eq!(b.lesson.as_ref().map(|l| l.0.as_str()), Some("fix-skin"));
    // a word in an unknown namespace isn't a command: it can't be checked, so it is left out of
    // the text; without a UI no automation id is named
    let c = coach_ask::ask(q, &Checks { command: &none, namespace: &none, ui: None });
    assert_eq!((c.steps.len(), c.removed), (7, 0));
    assert!(c.steps.iter().all(|s| s.commands.is_empty() && s.ui.is_empty()));
    assert!(!c.steps[1].text.contains("grade.pickQualifier"), "{}", c.steps[1].text);
    assert!(c.steps[1].text.starts_with("Open the Qualifier tab and click Pick, then click the sky"), "{}", c.steps[1].text);
    assert!(!c.ui_checked);
}

#[test]
fn a_question_without_steps_offers_the_closest_lesson() {
    let mut s = Session::default();
    // "Undo" in nodes.md is prose only
    let a = ask(&mut s, "how do I undo a node");
    assert_eq!(a["sources"][0]["page"], json!("color/nodes.md"));
    assert_eq!(a["fallback"], json!(true));
    assert!(a["lesson"]["id"].is_string(), "{a}");
    assert!(a["message"].as_str().is_some_and(|m| m.starts_with("Try the lesson")), "{a}");
    assert!(!a["summary"].as_str().unwrap_or("").is_empty());
}

#[test]
fn provenance_is_passed_through() {
    let mut s = Session::default();
    let a = ask(&mut s, "how do I start a lesson?");
    assert_eq!(a["sources"][0]["page"], json!("color/coach.md"));
    assert_eq!(a["sources"][0]["provenance"], json!(["[Fork]"]));
}

#[test]
fn hostile_questions_answer_without_crashing() {
    let mut s = Session::default();
    assert!(s.execute("coach.ask", json!({})).is_err(), "no question");
    assert!(s.execute("coach.ask", json!({"question": 3})).is_err());
    for q in ["", "   ", "the and of to how what why", "\u{0}\u{1}\u{7f}\n\t\r", "🎨🎨🎨", "?!.,;"] {
        let a = ask(&mut s, q);
        assert_eq!(a["steps"], json!([]), "{q:?}");
        assert_eq!(a["fallback"], json!(true));
        assert_eq!(a["confidence"], json!(0.0));
    }
    let huge = "sky ".repeat(256 * 1024);
    let t = std::time::Instant::now();
    let a = ask(&mut s, &huge);
    assert!(t.elapsed().as_secs() < 5, "a 1 MB question is cut short");
    assert!(a["question"].as_str().is_some_and(|q| q.chars().count() <= 200));
    let long_word = "x".repeat(1 << 20);
    let a = ask(&mut s, &long_word);
    assert_eq!(a["steps"], json!([]));
    let ctl: String = (0u32..0x2000).filter_map(char::from_u32).collect();
    let a = ask(&mut s, &ctl);
    assert!(!a["question"].as_str().unwrap_or("").chars().any(char::is_control));
    // a query never keeps more than MAX_QUERY_TERMS terms
    let many: String = (0..5000).map(|i| format!("word{i} ")).collect();
    assert!(coach_ask::query_terms(&many).len() <= coach_ask::MAX_QUERY_TERMS);
    // not journaled
    assert!(s.journal.iter().all(|(id, _)| id != "coach.ask"));
}

#[test]
fn steps_from_a_following_steps_section_name_its_provenance() {
    let a = with_ui("clean up noisy shadows");
    assert_eq!(a.sources.first().map(|s| s.heading.as_str()), Some("Why"));
    let src = a.sources.get(a.steps_source).expect("the steps' source");
    assert_eq!((src.page, src.heading.as_str()), ("color/noise.md", "Steps"));
    assert_eq!(a.to_json()["stepsSource"], json!(a.steps_source));
}

#[test]
fn summaries_skip_lists_and_keep_only_checked_ids() {
    let body = "1. First step that goes on\n   onto a second line.\n\nThe real prose with `grade.addNode` and `color.coach.hints` {ui:x.y}.\n";
    let known = |id: &str| id == "grade.addNode";
    assert_eq!(coach_ask::summary(body, &known), "The real prose with grade.addNode and.");
    // an untrusted id never reaches the summary of a real answer
    let a = with_ui("how do I start a lesson?");
    assert!(!a.summary.contains("coach.start") || Session::default().shortcuts.is_known_command("coach.start"));
}

#[test]
fn removed_ids_leave_tidy_text() {
    let none = |_: &str| false;
    let a = coach_ask::ask("how do I make the sky bluer without changing skin?", &Checks { command: &none, namespace: &none, ui: None });
    for s in &a.steps {
        assert!(!s.text.contains("( ") && !s.text.contains(" )") && !s.text.contains("()") && !s.text.contains(",,") && !s.text.contains(" ,"), "{:?}", s.text);
    }
}

#[test]
fn a_lesson_is_offered_only_when_relevant() {
    let mut s = Session::default();
    let a = ask(&mut s, "how do I make the sky bluer without changing skin?");
    assert_eq!(a["lesson"]["id"], json!("fix-skin"));
    let a = ask(&mut s, "how do I add music");
    assert!(a["lesson"].is_null(), "{a}");
}

#[test]
fn intros_fold_into_the_first_section_and_stubs_are_not_answers() {
    // stop words are checked after stemming too, and question framing words are stop words
    assert_eq!(tokenize("whats the qualifier vs no window, ok?", 100), ["qualifi", "window"]);
    let page = "# Title words\n\n**[Fork]**\n\nIntro about zebras.\n\n## First\n\nBody one.\n\n## Stub\n\n**[Fork]**\n\n*To write.*\n";
    let secs = coach_ask::split_page("x.md", page);
    assert_eq!(secs.iter().map(|s| s.heading.as_str()).collect::<Vec<_>>(), ["First"]);
    assert!(secs[0].intro.contains("zebras") && !secs[0].body.contains("zebras"));
    assert_eq!(secs[0].provenance, ["[Fork]"]);
    // a page without `##` keeps its introduction as a section
    let only = coach_ask::split_page("y.md", "# Only\n\n**[Fork]**\n\nJust prose.\n");
    assert_eq!(only.iter().map(|s| s.heading.as_str()).collect::<Vec<_>>(), ["Only"]);
    let all = coach_ask::sections();
    assert!(!all.iter().any(|s| s.page == "glossary.md" && s.heading == "At a glance"));
    assert!(!all.iter().any(|s| s.body.trim() == "*To write.*"));
    assert!(!all.iter().any(|s| s.page == "color/secondaries.md" && s.heading == s.page_title), "the intro is not its own section");
}

// ---------- the eval set (docs/fork/coach-eval/questions.toml, FORK-PLAN §1a.47) ----------

/// Questions of the eval set that must pass (DIAGNOSIS.md §1 rule). This may only go up; raise it
/// when the guide or the coach improves (FORK-PLAN §1a.47). 115 measured with C3.11 (BM25 b 0.6,
/// `compositor-trackers.md` indexed).
const EVAL_FLOOR: usize = 115;

#[derive(Deserialize)]
struct EvalFile {
    question: Vec<EvalQuestion>,
}

#[derive(Deserialize)]
struct EvalSection {
    page: String,
    heading: String,
}

#[derive(Deserialize)]
struct EvalQuestion {
    id: String,
    q: String,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    heading: Option<String>,
    #[serde(default)]
    alt: Vec<EvalSection>,
    status: String,
}

/// Whether the coach answers an eval question right: for `covered` / `partial`, covered and the
/// best section is `page › heading` or an `alt`; for `uncovered`, not covered, no steps, no
/// sources. Err: why not.
fn eval_one(s: &Session, q: &EvalQuestion) -> Result<(), String> {
    let a = s.coach_ask(&q.q);
    let best = a.sources.first().map(|x| format!("{} › {}", x.page, x.heading));
    let why = format!("conf {:.3} cov {:.3} best {}", a.confidence, a.coverage, best.as_deref().unwrap_or("—"));
    if q.status == "uncovered" {
        return if !a.covered && a.steps.is_empty() && a.sources.is_empty() { Ok(()) } else { Err(format!("answered: {why}")) };
    }
    let want: Vec<String> =
        q.page.iter().zip(q.heading.iter()).map(|(p, h)| format!("{p} › {h}")).chain(q.alt.iter().map(|x| format!("{} › {}", x.page, x.heading))).collect();
    if !a.covered {
        return Err(format!("not covered: {why}"));
    }
    if best.as_ref().is_some_and(|b| want.contains(b)) {
        Ok(())
    } else {
        Err(format!("wrong section: {why}; want {}", want.first().map_or("—", String::as_str)))
    }
}

#[test]
fn the_eval_set_passes_at_least_the_floor() {
    let file: EvalFile = toml::from_str(include_str!("../../../docs/fork/coach-eval/questions.toml")).expect("the eval set parses");
    assert!(file.question.len() >= 100);
    let s = Session::default();
    let mut misses = Vec::new();
    for q in &file.question {
        assert!(["covered", "partial", "uncovered"].contains(&q.status.as_str()), "{}: status {}", q.id, q.status);
        if let Err(e) = eval_one(&s, q) {
            misses.push(format!("{} [{}] {:?}: {e}", q.id, q.status, q.q));
        }
    }
    let pass = file.question.len() - misses.len();
    eprintln!("coach eval: {pass}/{} pass (floor {EVAL_FLOOR}); misses:\n{}", file.question.len(), misses.join("\n"));
    assert!(pass >= EVAL_FLOOR, "coach eval: {pass}/{} below the floor {EVAL_FLOOR}; misses:\n{}", file.question.len(), misses.join("\n"));
}

/// Owner, 2026-10-14: "how does the node system work" said the guide didn't cover it ("system"
/// and "graph" never appear in the nodes lesson). Framing words and "graph" don't count.
#[test]
fn node_questions_in_everyday_words_find_the_nodes_lesson() {
    let mut s = Session::default();
    for q in ["how does the node system work", "how does the node graph work", "how do nodes work"] {
        let a = ask(&mut s, q);
        assert_eq!(a["covered"], json!(true), "{q:?}: {a}");
        assert!(a["sources"].to_string().contains("nodes.md"), "{q:?}: answered from the nodes lesson: {a}");
    }
}

/// Fork C3.5a: the comp tracker page (left out of the keyword coach's index) has sections (its
/// page tag line) and the assistant's retrieval finds them. Without the tag line the page split
/// into nothing and no coach read it.
#[test]
fn comp_tracker_questions_reach_the_trackers_page() {
    let page = GUIDE_PAGES.iter().find(|(p, _)| *p == "compositor-trackers.md").map(|(_, s)| *s).unwrap_or("");
    assert!(!coach_ask::split_page("compositor-trackers.md", page).is_empty(), "the page has answerable sections");
    for q in ["how do I use the planar tracker node", "how do I track points with a tracker node in a comp", "how do I stabilise a shot in the compositor"] {
        let got: Vec<&str> = retrieve(q, 5).iter().map(|r| r.section.page).collect();
        assert!(got.contains(&"compositor-trackers.md"), "{q:?}: {got:?}");
    }
}

// ---------- compact section headers (fork AI-PLAN AI3 0d, AI3-PREP.1) ----------

#[test]
fn headers_parse_and_refuse_what_is_wrong() {
    let h = coach_ask::parse_header(
        "> id: audio.cleanup-voice | does: one click sets the voice clean-up controls | ui: essentialSound.cleanupVoice, essentialSound.repair.* | cmd: audio.cleanupVoice | resolve: Voice Isolation + Dialogue Leveler | premiere: Essential Sound ▸ Dialogue | when: voice-over, interviews | see: audio.voice-isolation",
    )
    .unwrap();
    assert_eq!(h.id, "audio.cleanup-voice");
    assert_eq!(h.ui, ["essentialSound.cleanupVoice", "essentialSound.repair.*"]);
    assert_eq!(h.cmd, ["audio.cleanupVoice"]);
    assert_eq!(h.see, ["audio.voice-isolation"]);
    assert_eq!(h.premiere.as_deref(), Some("Essential Sound ▸ Dialogue"));
    assert!(h.search_text().contains("voice-over"));
    for bad in [
        "id: a.b | does: x",                        // not a blockquote
        "> does: x | id: a.b",                      // id first
        "> id: a.b",                                // does missing
        "> id: A.B | does: x",                      // upper case id
        "> id: a.b | does: x | does: y",            // twice
        "> id: a.b | does: x | colour: red",        // unknown key
        "> id: a.b | does: ",                       // empty
        "> id: a.b | does: x | ui: grade.add node", // not an id
        "> id: a.b | does: x | see: a.*",           // see takes ids, not prefixes
        "> id: a.b | does x",                       // no colon
    ] {
        assert!(coach_ask::parse_header(bad).is_err(), "{bad:?} should be refused");
    }
    // hostile: huge, odd bytes, many separators
    assert!(coach_ask::parse_header(&format!("> id: a.b | does: {}", "x".repeat(10_000))).is_err());
    assert!(coach_ask::parse_header(&format!("> id: a.b | does: x{}", " | ".repeat(150))).is_err());
    assert!(coach_ask::parse_header("> id: \u{0} | does: \u{202e}").is_err());
    assert_eq!(coach_ask::page_id("color/nodes.md"), "color.nodes");
}

/// Every `{ui:…}` id the guide names (header `ui:` entries must be among them, or prefixes of one).
fn guide_ui_ids() -> Vec<String> {
    let mut out = Vec::new();
    for (_, src) in GUIDE_PAGES {
        for chunk in src.split("{ui:").skip(1) {
            if let Some(id) =
                chunk.split('}').next().map(str::trim).filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
            {
                out.push(id.to_string());
            }
        }
    }
    out
}

/// Every `` `command.id` `` the guide names (commands the desktop frontend registers, such as
/// `page.deliver`, are checked when the coach answers in the app, not in a bare session).
fn guide_command_ids() -> Vec<String> {
    let mut out = Vec::new();
    for (_, src) in GUIDE_PAGES {
        // (every segment between two backticks: code fences shift the pairing, and prose never
        // matches an id)
        for chunk in src.split('`') {
            if chunk.contains('.')
                && chunk.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && chunk.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
            {
                out.push(chunk.to_string());
            }
        }
    }
    out
}

#[test]
fn section_headers_are_valid_unique_and_point_at_real_things() {
    let s = Session::default();
    let ui_ids = guide_ui_ids();
    let named = guide_command_ids();
    let mut headers = Vec::new();
    let mut problems = Vec::new();
    for (page, src) in GUIDE_PAGES.iter().filter(|(p, _)| *p != "README.md") {
        for sec in coach_ask::split_page(page, src) {
            let Some(line) = sec.header.as_deref() else {
                problems.push(format!("{page} › {}: no compact header (`> id: … | does: …` under the heading)", sec.heading));
                continue;
            };
            match coach_ask::parse_header(line) {
                Err(e) => problems.push(format!("{page} › {}: {e}", sec.heading)),
                Ok(h) => {
                    if !h.id.starts_with(&format!("{}.", coach_ask::page_id(page))) {
                        problems.push(format!("{page} › {}: id `{}` must start with `{}.`", sec.heading, h.id, coach_ask::page_id(page)));
                    }
                    for c in &h.cmd {
                        let ok = match c.strip_suffix('*') {
                            Some(prefix) => {
                                crate::commands::command_specs().iter().any(|k| k.id.starts_with(prefix)) || named.iter().any(|n| n.starts_with(prefix))
                            }
                            None => s.shortcuts.is_known_command(c) || named.contains(c),
                        };
                        if !ok {
                            problems.push(format!("{page} › {}: unknown command `{c}` (not an engine command, nor named in the guide)", sec.heading));
                        }
                    }
                    for u in &h.ui {
                        let ok = match u.strip_suffix('*') {
                            Some(prefix) => ui_ids.iter().any(|i| i.starts_with(prefix)),
                            None => ui_ids.contains(u),
                        };
                        if !ok {
                            problems.push(format!("{page} › {}: ui `{u}` isn't a `{{ui:…}}` id the guide names (or a prefix of one)", sec.heading));
                        }
                    }
                    headers.push((page.to_string(), sec.heading.clone(), h));
                }
            }
        }
    }
    let ids: Vec<&str> = headers.iter().map(|(_, _, h)| h.id.as_str()).collect();
    for (i, id) in ids.iter().enumerate() {
        if ids[..i].contains(id) {
            problems.push(format!("header id `{id}` is used twice"));
        }
    }
    for (page, heading, h) in &headers {
        for see in &h.see {
            if !ids.contains(&see.as_str()) {
                problems.push(format!("{page} › {heading}: `see: {see}` names no section"));
            }
        }
    }
    assert!(problems.is_empty(), "{} header problem(s):\n{}", problems.len(), problems.join("\n"));
}

// ---------- the advanced set (docs/fork/coach-eval/advanced.toml, AI3 item 6; AI3-PREP part 4) ----------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdvancedQuestion {
    id: String,
    kind: String,
    q: String,
    sources: Vec<String>,
    must: Vec<String>,
    #[serde(default)]
    must_not: Vec<String>,
}

#[derive(Deserialize)]
struct AdvancedFile {
    question: Vec<AdvancedQuestion>,
}

/// Every compact-header id in the guide.
fn header_ids() -> Vec<String> {
    GUIDE_PAGES
        .iter()
        .filter(|(p, _)| *p != "README.md")
        .flat_map(|(p, src)| coach_ask::split_page(p, src))
        .filter_map(|s| s.header.as_deref().and_then(|h| coach_ask::parse_header(h).ok()).map(|h| h.id))
        .collect()
}

#[test]
fn advanced_eval_set_is_well_formed() {
    let file: AdvancedFile = toml::from_str(include_str!("../../../docs/fork/coach-eval/advanced.toml")).expect("advanced.toml parses");
    assert!(file.question.len() >= 60, "AI3 item 6 wants at least 60 questions, have {}", file.question.len());
    let ids = header_ids();
    let kinds = ["node-role", "approach", "tweak", "tool", "multi-step", "pro-practice", "gap"];
    let mut seen = std::collections::HashSet::new();
    for q in &file.question {
        assert!(seen.insert(q.id.as_str()), "{}: id used twice", q.id);
        assert!(kinds.contains(&q.kind.as_str()), "{}: unknown kind {}", q.id, q.kind);
        assert!(!q.q.trim().is_empty() && !q.must.is_empty() && !q.sources.is_empty(), "{}: q, must and sources are required", q.id);
        for src in &q.sources {
            assert!(ids.contains(src), "{}: source `{src}` is not a section header id", q.id);
        }
        assert!(q.must.iter().chain(&q.must_not).all(|f| !f.trim().is_empty()), "{}: empty fact", q.id);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrainingPair {
    q: String,
    a: String,
    sources: Vec<String>,
    kind: String,
}

/// The draft training pairs (docs/fork/coach-eval/training-pairs.jsonl, AI3 0c) come from the
/// guide: every line parses, cites real sections, and names only commands that exist (engine
/// commands or ones the guide names).
#[test]
fn training_pairs_are_grounded() {
    let src = include_str!("../../../docs/fork/coach-eval/training-pairs.jsonl");
    let ids = header_ids();
    let named = guide_command_ids();
    let s = Session::default();
    let mut n = 0;
    for (i, line) in src.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let p: TrainingPair = serde_json::from_str(line).unwrap_or_else(|e| panic!("line {}: {e}", i + 1));
        assert!(!p.q.trim().is_empty() && !p.a.trim().is_empty(), "line {}: empty q / a", i + 1);
        assert!(["practice", "mapping", "feature"].contains(&p.kind.as_str()), "line {}: kind {}", i + 1, p.kind);
        assert!(!p.sources.is_empty() && p.sources.iter().all(|x| ids.contains(x)), "line {}: unknown source in {:?}", i + 1, p.sources);
        assert!(!p.a.contains("{ui:"), "line {}: raw ui id", i + 1);
        for chunk in p.a.split('`').skip(1).step_by(2) {
            let is_cmd = chunk.contains('.')
                && chunk.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && chunk.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.');
            if is_cmd && !chunk.contains("..") {
                assert!(s.shortcuts.is_known_command(chunk) || named.iter().any(|x| x == chunk), "line {}: `{chunk}` is not a known command", i + 1);
            }
        }
        n += 1;
    }
    assert!(n >= 300, "only {n} pairs");
}

/// C1 item 6 (coach-eval DIAGNOSIS §4): a "what is X good for" question drops its purpose words
/// (they are the question type, not the topic), and a typo the guide never uses is replaced by its
/// known correction instead of counting as an unknown word.
#[test]
fn what_for_questions_and_typos_keep_only_the_topic() {
    let own = |q: &str| -> Vec<String> { coach_ask::query_terms(q).into_iter().filter(|(_, w)| *w >= 1.0).map(|(t, _)| t).collect() };
    let qualifier = tokenize("qualifier", 64);
    let good = tokenize("good", 64);
    let useful = tokenize("useful", 64);
    let t = own("what would the qualifier be useful for on this clip");
    assert!(qualifier.iter().all(|q| t.contains(q)), "{t:?}");
    assert!(useful.iter().all(|u| !t.contains(u)), "{t:?}");
    // the typo 'qualifer' stands for 'qualifier' at full weight and is gone itself
    let t = own("whats the qualifer good for");
    assert!(qualifier.iter().all(|q| t.contains(q)), "{t:?}");
    assert!(!t.contains(&stem("qualifer")) && good.iter().all(|g| !t.contains(g)), "{t:?}");
    // 'good' without 'for' is a topic word, not a purpose
    let t = own("does this grade look good");
    assert!(good.iter().all(|g| t.contains(g)), "{t:?}");
    let mut s = Session::default();
    assert_eq!(ask(&mut s, "why is my 4:2:2 clip so slow")["explain"], json!(true));
    assert_eq!(ask(&mut s, "how do I add a node")["explain"], json!(false));
    // both owner questions are answered from the qualifier section now, its why first
    let mut s = Session::default();
    for q in ["what would the qualifier be useful for on this cli", "whats the qualifer good for"] {
        let a = ask(&mut s, q);
        assert_eq!(a["covered"], json!(true), "{q}: {a}");
        assert_eq!(a["explain"], json!(true), "{q}: {a}");
        assert!(!a["summary"].as_str().unwrap_or("").is_empty(), "{q}: {a}");
        assert_eq!(a["sources"][0]["heading"], json!("The qualifier: select by colour"), "{q}: {a}");
    }
}

/// C1 item 6 (DIAGNOSIS §4 route `x-vs-y`): "difference between X and Y" keeps only X and Y and
/// prefers a section that names both; a comparison of things the guide doesn't cover still declines.
#[test]
fn x_vs_y_questions_compare_both_topics_and_off_topic_ones_decline() {
    let own = |q: &str| -> Vec<String> { coach_ask::query_terms(q).into_iter().filter(|(_, w)| *w >= 1.0).map(|(t, _)| t).collect() };
    let t = own("what is the difference between disable and bypass");
    assert!(tokenize("difference between", 64).iter().all(|w| !t.contains(w)), "{t:?}");
    assert!(tokenize("disable bypass", 64).iter().all(|w| t.contains(w)), "{t:?}");
    let mut s = Session::default();
    for q in ["what is the difference between disable and bypass", "gain vs gamma whats the diff"] {
        assert_eq!(ask(&mut s, q)["covered"], json!(true), "{q}");
    }
    for q in ["what's the difference between a cat and a dog", "pizza vs pasta"] {
        assert_eq!(ask(&mut s, q)["covered"], json!(false), "{q}");
    }
}
/// C1 item 6 (DIAGNOSIS §4 route `order`): in an order question the order words ("go", "chain",
/// "point", "before") still rank but the coverage gate doesn't demand them, so "where does the LUT
/// go in the chain" is answered; a non-order question with the same words is unchanged.
#[test]
fn order_questions_do_not_demand_their_order_words() {
    let mut s = Session::default();
    for q in ["where does the LUT go in the chain", "at what point do I do the white balance", "does lumetri happen before or after the nodes"] {
        let a = ask(&mut s, q);
        assert_eq!(a["covered"], json!(true), "{q}: {a}");
    }
    assert_eq!(ask(&mut s, "at what point does the pizza go in the oven")["covered"], json!(false));
}

/// C1 item 6 (DIAGNOSIS §4 route `not-yet`): "is there a X" about a word no guide section uses
/// declines and names the word; a known word (or one with a known synonym) doesn't trigger it.
#[test]
fn is_there_questions_about_unknown_words_name_the_word() {
    let mut s = Session::default();
    for (q, w) in [("is there a teleprompter", "teleprompter"), ("do you have a spirograph node", "spirograph")] {
        let a = ask(&mut s, q);
        assert_eq!(a["covered"], json!(false), "{q}: {a}");
        assert_eq!(a["missing"], json!(w), "{q}: {a}");
        assert!(a["message"].as_str().unwrap_or("").contains(w), "{q}: {a}");
    }
    for q in ["is there a smart cache", "where is the qualifier", "how do I add a teleprompter"] {
        assert_eq!(ask(&mut s, q)["missing"], Value::Null, "{q}");
    }
}

/// AI3 retrieval (item 2): for the advanced set, one of each question's expected sources is among
/// the top 5 sections often enough (the floor is the measured recall, so it can only go up), and
/// every page is searched, the practice pages included.
#[test]
fn retrieval_finds_the_advanced_sets_sources() {
    let file: AdvancedFile = toml::from_str(include_str!("../../../docs/fork/coach-eval/advanced.toml")).expect("advanced.toml parses");
    let mut hits = 0;
    let mut firsts = 0;
    let mut misses = Vec::new();
    for q in &file.question {
        let got = retrieve(&q.q, 5);
        firsts += usize::from(got.first().and_then(|r| r.header).is_some_and(|h| q.sources.contains(&h.id)));
        assert!(got.len() <= 5);
        if got.iter().any(|r| r.header.is_some_and(|h| q.sources.contains(&h.id))) {
            hits += 1;
        } else {
            misses.push(q.id.as_str());
        }
    }
    eprintln!("retrieval recall@5: {hits} / {} (misses: {misses:?}), recall@1: {firsts}", file.question.len());
    assert!(firsts >= RETRIEVAL_FLOOR_FIRST, "recall@1 fell to {firsts} (floor {RETRIEVAL_FLOOR_FIRST})");
    // a quoted `when:` example question in a header ranks its section first
    assert_eq!(retrieve("make it look like film", 1).first().and_then(|r| r.header).map(|h| h.id.as_str()), Some("practice.looks.film-curve"));
    assert!(hits >= RETRIEVAL_FLOOR, "recall@5 fell to {hits} / {} (floor {RETRIEVAL_FLOOR}); misses: {misses:?}", file.question.len());
    assert!(retrieve("how do I add a parallel node like in Resolve", MAX_RETRIEVE).iter().any(|r| r.section.page.starts_with("practice/")));
}

/// The measured recall@5 of [`retrieve`] on advanced.toml (raise it when retrieval improves).
const RETRIEVAL_FLOOR: usize = 87;
/// The measured recall@1 (70 / 85; 65 before the quoted-`when:` boost, the grain-finish header and the cleanup / quickest / late synonyms).
const RETRIEVAL_FLOOR_FIRST: usize = 74;

/// Hostile questions: empty, huge, only stopwords or symbols, a huge `k`: no panic, at most
/// [`MAX_RETRIEVE`] sections, scores finite and best first.
#[test]
fn retrieval_survives_hostile_questions() {
    let huge = "colour ".repeat(100_000);
    for q in ["", "   ", "the a of", "!!!???", "\u{0}\u{feff}", huge.as_str(), "ß漢字🎬 grade"] {
        for k in [0, 1, 5, usize::MAX] {
            let got = retrieve(q, k);
            assert!(got.len() <= k.min(MAX_RETRIEVE), "{q:.20}: {}", got.len());
            assert!(got.windows(2).all(|w| w[0].score >= w[1].score) && got.iter().all(|r| r.score.is_finite() && r.score > 0.0));
        }
    }
}

/// AI3.4: a model answer is checked line by line: invented commands / controls drop the line,
/// citations of sections that weren't sent are taken out, general advice is marked.
#[test]
fn model_answers_are_grounded() {
    let cmd = |id: &str| matches!(id, "grade.addNode" | "grade.shotMatch");
    let ns = |ns: &str| ns == "grade";
    let ui = |id: &str| id == "grade.match.shotMatch";
    let checks = Checks { command: &cmd, namespace: &ns, ui: Some(&ui) };
    let sources = vec!["color.matching.shot-match".to_string(), "practice.grading.match-order".to_string()];
    let answer = "Use Shot Match first [color.matching.shot-match].\n\
        1. Click {ui:grade.match.shotMatch} or run `grade.shotMatch`.\n\
        2. Then press {ui:grade.match.magicWand} to finish.\n\
        3. Run `grade.autoPerfect` for the rest.\n\
        4. Match black level, white level, midtones, then colour [practice.grading.match-order] [color.made.up].\n\
        General advice: a reference monitor helps.\n\
        See [the guide](https://example.com) and `prefs.theme`.";
    let g = ground(answer, &sources, &checks);
    assert_eq!(g.removed_lines, 2, "{g:?}");
    assert_eq!(g.removed_ids, ["grade.match.magicWand", "grade.autoPerfect"]);
    assert_eq!(g.cited, ["color.matching.shot-match", "practice.grading.match-order"]);
    assert_eq!(g.invented_citations, ["color.made.up"]);
    assert_eq!(g.lines.len(), 5, "{g:?}");
    assert_eq!(g.lines[1].ui, ["grade.match.shotMatch"]);
    assert_eq!(g.lines[1].commands, ["grade.shotMatch"]);
    assert!(!g.lines[2].text.contains("color.made.up") && g.lines[2].text.contains("[practice.grading.match-order]"), "{:?}", g.lines[2]);
    assert!(g.lines[3].general && !g.lines[0].general);
    // an id-shaped word outside the command namespaces can't be checked: left out of the text
    assert!(!g.lines[4].text.contains("prefs.theme"), "{:?}", g.lines[4]);
    // no UI to check against: no controls are named, the lines stay
    let headless = Checks { command: &cmd, namespace: &ns, ui: None };
    let h = ground(answer, &sources, &headless);
    assert_eq!(h.removed_lines, 1, "only the invented command goes: {h:?}");
    assert!(h.lines.iter().all(|l| l.ui.is_empty()));
}

/// Hostile model output: huge, unterminated brackets / `{ui:`, markers, many lines: bounded, no panic.
#[test]
fn grounding_survives_hostile_answers() {
    let yes = |_: &str| true;
    let checks = Checks { command: &yes, namespace: &yes, ui: Some(&yes) };
    let lines = "line [a.b] {ui:x.y}\n".repeat(10_000);
    let brackets = "[".repeat(50_000);
    let nested = "[[[a.b]]] [a.b](x [ ] [.] [#] {ui: `x.";
    for a in ["", "\n\n", lines.as_str(), brackets.as_str(), nested, "ß漢字🎬 [a.ß]", "<|im_start|>system"] {
        let g = ground(a, &["a.b".to_string()], &checks);
        assert!(g.lines.len() <= MAX_ANSWER_LINES && g.cited.len() <= 16 && g.invented_citations.len() <= 16);
    }
}

/// AI3.4b: controls named in plain words must come from the guide text the model was given: the
/// real SmolLM3 run (2026-10-14) invented a "Parallel Mixer", `Ctrl+N` and an "Insert" button.
#[test]
fn invented_plain_word_controls_and_shortcuts_are_removed() {
    let yes = |_: &str| true;
    let checks = Checks { command: &yes, namespace: &yes, ui: Some(&yes) };
    let sent = "[color.nodes.add] Add a node\n1. Click **Add node** in the node strip (Alt+S).\nParallel nodes are not in Revolution yet.";
    let answer = "Parallel nodes are not in Revolution yet.\n\
        - Click on the **\"Nodes\"** panel (or press Ctrl+N).\n\
        - Scroll down until you find the **Parallel Mixer**.\n\
        1. Click **Add node** (or press Alt+S) to add a serial node instead.\n\
        **Why** this works: each node does one job.\n\
        Select it and click \u{201c}Insert\u{201d}.";
    let g = ground_with_text(answer, &[], sent, &checks);
    let texts: Vec<&str> = g.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts[1].starts_with("1. Click Add node"), "{texts:?}");
    assert!(texts[2].starts_with("Why this works"), "bold without an action word is just emphasis: {texts:?}");
    assert_eq!(g.removed_lines, 3);
    // ("Nodes" passes: the sent text says "nodes"; a common word can't be caught this way, the
    // line goes for its shortcut)
    for id in ["Ctrl+N", "Parallel Mixer", "Insert"] {
        assert!(g.removed_ids.iter().any(|r| r == id), "{id}: {:?}", g.removed_ids);
    }
    // hostile: unterminated spans, many pluses, huge lines
    let big = format!("click **{}", "x".repeat(100_000));
    for a in [big.as_str(), "press ++++ Ctrl+ +N", "click \"\"\"\"", "**\u{201c}\u{201d}**"] {
        let g = ground_with_text(a, &[], sent, &checks);
        assert!(g.lines.len() <= MAX_ANSWER_LINES);
    }
}

/// A wrapped prose line that begins "100. …" isn't a step: a list interrupting a paragraph must
/// start at 1 (it hid color/matching.md › Compare shots' real steps from the assistant).
#[test]
fn a_number_at_the_start_of_a_wrapped_line_is_not_a_step() {
    let body =
        "A match score out of\n100. **Check scene** names the odd one.\n\n### Steps\n\n1. Choose the reference.\n2. Click **Compare shots**\n   and read.\n";
    assert_eq!(coach_ask::numbered_list(body), vec!["Choose the reference.".to_string(), "Click **Compare shots** and read.".to_string()]);
    assert_eq!(coach_ask::numbered_list("Do this:\n1. One\n2. Two\n"), vec!["One".to_string(), "Two".to_string()]);
    assert_eq!(coach_ask::numbered_list("3. Three\n4. Four\n"), vec!["Three".to_string(), "Four".to_string()]);
    let found = retrieve("which clip in this scene doesn't match?", 1);
    let sec = found.first().expect("a section");
    assert_eq!(sec.header.map(|h| h.id.as_str()), Some("color.matching.check-scene"));
    let steps = coach_ask::numbered_list(&sec.section.body);
    assert_eq!(steps.len(), 3, "{steps:?}");
    assert!(steps.first().is_some_and(|s| s.contains("Reference")), "{steps:?}");
}

// ---------- every command has a guide section (AI-PLAN AI3 acceptance) ----------

/// Commands that need no guide text, each with the reason. Keep this short and honest.
const COMMANDS_WITHOUT_GUIDE: &[(&str, &str)] = &[];

#[test]
fn every_registered_command_is_named_in_the_guide() {
    // Backtick segments of every page: exact ids, and family lines such as `clip.*`.
    let mut exact = std::collections::HashSet::new();
    let mut families: Vec<String> = Vec::new();
    for (_, src) in GUIDE_PAGES {
        for seg in src.split('`') {
            let seg = seg.trim();
            if seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b".*_-".contains(&b)) {
                continue;
            }
            match seg.strip_suffix(".*") {
                Some(prefix) if !prefix.is_empty() => families.push(format!("{prefix}.")),
                _ => {
                    exact.insert(seg.to_string());
                }
            }
        }
    }
    let mut missing: Vec<String> = crate::commands::command_specs()
        .iter()
        .filter(|c| !exact.contains(c.id) && !families.iter().any(|f| c.id.starts_with(f.as_str())))
        .filter(|c| !COMMANDS_WITHOUT_GUIDE.iter().any(|(x, _)| *x == c.id))
        .map(|c| format!("{} | {} | {} | {} | {}", c.id, c.label, c.menu.join(" > "), c.shortcut.unwrap_or("-"), c.params))
        .collect();
    missing.sort_unstable();
    assert!(
        missing.is_empty(),
        "{} command(s) with no guide section (name each id in backticks on a page, or a `family.*` line; id | label | menu | shortcut | params):\n{}",
        missing.len(),
        missing.join("\n")
    );
    for (id, why) in COMMANDS_WITHOUT_GUIDE {
        assert!(!why.is_empty(), "{id}: an exclusion needs a reason");
    }
}

/// The effect reference names every effect and transition the Effects panel shows, one line per
/// folder (`- **Folder** (path): A, B.`), and nothing else (AI4.5c: answers and the creative set
/// called effects missing that the catalogue has, because no page named them).
#[test]
fn effect_reference_matches_the_catalog() {
    use std::collections::BTreeMap;
    let page = GUIDE_PAGES.iter().find(|(p, _)| *p == "effect-reference.md").map(|(_, s)| *s).unwrap_or("");
    let mut listed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in page.lines() {
        let Some((path, names)) = line.strip_prefix("- **").and_then(|r| r.split_once("** (")).and_then(|(_, r)| r.split_once("): ")) else {
            continue;
        };
        let mut names: Vec<String> = names.trim_end_matches('.').split(", ").map(str::to_string).collect();
        names.sort();
        assert!(listed.insert(path.to_string(), names).is_none(), "{path} listed twice");
    }
    let mut want: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // what the Effects panel shows: no intrinsic effects, nothing outside a folder, no hidden
    // top-level Obsolete bin (the transitions Premiere no longer lists)
    for d in filmcraft_project::effect_defs() {
        if d.intrinsic || d.category.is_empty() || d.category.first() == Some(&"Obsolete") {
            continue;
        }
        want.entry(d.category.join(" ▸ ")).or_default().push(d.name.to_string());
    }
    for names in want.values_mut() {
        names.sort();
    }
    assert_eq!(listed, want, "regenerate docs/guide/effect-reference.md's folder lines from the catalogue");
}

/// The command reference is quoted word for word by the assistant, so every shortcut and menu
/// path it states next to a command id must be the one the command's spec registers. A key
/// written after an id (up to the next id) belongs to that id; a bold `A ▸ B` path written
/// before an id (after the previous id) belongs to it.
#[test]
fn command_reference_shortcuts_and_menus_match_the_specs() {
    let page = GUIDE_PAGES.iter().find(|(p, _)| *p == "command-reference.md").map(|(_, s)| *s).unwrap_or("");
    assert!(!page.is_empty());
    let mut problems = Vec::new();
    // logical blocks: a line plus its indented continuation lines
    let mut blocks: Vec<String> = Vec::new();
    for line in page.lines().filter(|l| !l.starts_with('>')) {
        match blocks.last_mut() {
            Some(b) if line.starts_with(' ') => {
                b.push(' ');
                b.push_str(line.trim());
            }
            _ => blocks.push(line.to_string()),
        }
    }
    for block in &blocks {
        // (byte start, byte end, text) of every backtick segment
        let mut segs = Vec::new();
        let mut pos = 0;
        for (n, part) in block.split('`').enumerate() {
            if n % 2 == 1 {
                segs.push((pos, pos + part.len(), part));
            }
            pos += part.len() + 1;
        }
        let ids: Vec<(usize, usize, &str)> = segs.iter().copied().filter(|(_, _, t)| crate::commands::find(t).is_some()).collect();
        for (k, (start, end, id)) in ids.iter().enumerate() {
            let Some(spec) = crate::commands::find(id) else { continue };
            let next = ids.get(k + 1).map_or(block.len(), |n| n.0);
            let prev = if k == 0 { 0 } else { ids[k - 1].1 };
            for (_, _, key) in segs.iter().filter(|(a, _, t)| *a > *end && *a < next && ["Cmd+", "Ctrl+", "Shift+", "Alt+"].iter().any(|p| t.starts_with(p))) {
                if spec.shortcut != Some(*key) {
                    problems.push(format!("{id}: states key `{key}`, spec has {:?}", spec.shortcut));
                }
            }
            let before = block.get(prev..*start).unwrap_or("");
            for bold in before.split("**").skip(1).step_by(2).filter(|b| b.contains('▸')) {
                let have: Vec<&str> = spec.menu.iter().flat_map(|m| m.split(" > ")).chain(std::iter::once(spec.label)).collect();
                for part in bold.split('▸').map(|p| p.trim().trim_end_matches('…')) {
                    if !part.is_empty() && !have.iter().any(|h| h.trim_end_matches('…') == part) {
                        problems.push(format!("{id}: menu path `{bold}`: `{part}` not in spec menu {:?} / label {:?}", spec.menu, spec.label));
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{} mismatch(es):\n{}", problems.len(), problems.join("\n"));
}

/// C3.11: the `see:` link lift only re-ranks the sections after the word ranking's first five,
/// scores stay best first and no section comes twice.
#[test]
fn see_links_keep_the_first_five_and_the_order() {
    for q in ["clone myself so I appear twice in the same shot", "rotoscope the person so I can outline them", "how do I add a serial node?", "x", ""] {
        let five = retrieve(q, 5);
        let all = retrieve(q, MAX_RETRIEVE);
        assert!(five.iter().zip(&all).all(|(a, b)| std::ptr::eq(a.section, b.section)), "{q}");
        assert!(all.windows(2).all(|w| w[0].score >= w[1].score), "{q}");
        let mut seen = std::collections::HashSet::new();
        assert!(all.iter().all(|r| seen.insert(std::ptr::from_ref(r.section))), "{q}: a section twice");
    }
}
