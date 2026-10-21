//! Fork AI-PLAN Phase REF: the reference answer (measured difference, recipe, on set, too
//! different, Do this). Fixture pictures are generated: a neutral "yours" and known looks.

use filmcraft_color::WorkingSpace;
use filmcraft_render::Image;
use filmcraft_render::look_diff::{LookFacts, LookTag};
use serde_json::json;

use crate::Session;
use crate::coach_reference::{RefAnswer, recipe};

/// A neutral picture: a smooth tonal ramp with a few colour patches (linear, 320 × 180).
fn neutral() -> Image {
    let (w, h) = (320usize, 180usize);
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let v = 0.01 + 0.8 * (x as f32 / w as f32).powf(2.0);
            let c = match (x / 80, y / 60) {
                (1, 1) => [0.45, 0.25, 0.17],
                (2, 2) => [0.1, 0.25, 0.1],
                (3, 0) => [0.1, 0.12, 0.3],
                _ => [v, v, v],
            };
            px.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
        }
    }
    Image { w, h, px }
}

fn map(img: &Image, f: impl Fn([f32; 3], usize, usize) -> [f32; 3]) -> Image {
    let mut out = img.clone();
    for (i, p) in out.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let c = f([p[0], p[1], p[2]], i % img.w, i / img.w);
        p[..3].copy_from_slice(&c);
    }
    out
}

fn facts(img: &Image) -> LookFacts {
    LookFacts::measure(img, WorkingSpace::Rec709).expect("measurable")
}

fn answer_for(theirs: &Image) -> RefAnswer {
    recipe(&facts(theirs), &facts(&neutral()), 7, 9)
}

fn guide_has_id(id: &str) -> bool {
    let key = format!("> id: {id} |");
    crate::coach_ask::GUIDE_PAGES.iter().any(|(_, src)| src.contains(&key))
}

/// Capitalised control names in a step ("Gain", "Pivot", "Crop") must be words the guide uses:
/// the recipe never names a control Revolution doesn't have.
fn assert_grounded(a: &RefAnswer) {
    let corpus: String = crate::coach_ask::GUIDE_PAGES.iter().map(|(_, src)| *src).collect::<Vec<_>>().join("\n");
    assert!(!a.grade.is_empty() && !a.on_set.is_empty(), "both parts, always: {a:?}");
    assert_eq!(a.grade[0].see, "color.matching.shot-match");
    for s in &a.grade {
        assert!(guide_has_id(s.see), "step points at a missing guide section: {}", s.see);
        for w in s.text.split(|c: char| !c.is_alphanumeric()).skip(1).filter(|w| w.chars().next().is_some_and(char::is_uppercase)) {
            assert!(corpus.contains(w), "`{w}` in \"{}\" is not a word of the guide", s.text);
        }
    }
    let text = a.text();
    assert!(text.contains("In the grade:") && text.contains("On set:"), "{text}");
    assert!(!text.contains("NaN") && !text.contains("inf"), "{text}");
}

fn has(a: &RefAnswer, t: LookTag) -> bool {
    a.tags.contains(&t)
}

#[test]
fn known_looks_are_named_and_answered_in_both_parts() {
    let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    // teal / orange: shadows towards cyan-blue, highlights towards orange
    let teal_orange = map(&neutral(), |c, _, _| {
        let l = luma(c);
        let k = (l / 0.8).clamp(0.0, 1.0);
        [c[0] * (0.8 + 0.5 * k), c[1] * (1.05 - 0.05 * k), c[2] * (1.3 - 0.7 * k)]
    });
    let a = answer_for(&teal_orange);
    assert!(has(&a, LookTag::TealShadows) || has(&a, LookTag::SplitToneTealOrange), "{:?}", a.tags);
    assert!(a.grade.iter().any(|s| s.see == "practice.grading.split-tone"), "{a:?}");
    assert_grounded(&a);
    // faded blacks
    let faded = map(&neutral(), |c, _, _| c.map(|v| 0.06 + 0.94 * v));
    let a = answer_for(&faded);
    assert!(has(&a, LookTag::FadedBlacks), "{:?}", a.tags);
    assert!(a.grade.iter().any(|s| s.see == "practice.looks.faded-blacks"));
    assert_grounded(&a);
    // warm highlights only
    let warm_high = map(&neutral(), |c, _, _| if luma(c) > 0.3 { [c[0] * 1.25, c[1] * 1.05, c[2] * 0.75] } else { c });
    let a = answer_for(&warm_high);
    assert!(has(&a, LookTag::OrangeHighlights) || has(&a, LookTag::WarmerOverall), "{:?}", a.tags);
    assert_grounded(&a);
    // letterbox 2.39:1 on 16:9
    let bar = ((180.0 - 320.0 / 2.39) / 2.0) as usize;
    let letterbox = map(&neutral(), |c, _, y| if y < bar || y >= 180 - bar { [0.0; 3] } else { c });
    let a = answer_for(&letterbox);
    assert!(has(&a, LookTag::Letterbox), "{:?}", a.tags);
    assert!(a.grade.iter().any(|s| s.text.contains("Crop")), "{a:?}");
    assert_grounded(&a);
    // heavy grain (deterministic noise)
    let mut seed = 12345u32;
    let mut noise = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32 * 0.5 - 0.25
    };
    let mut grainy = neutral();
    for p in grainy.px.as_chunks_mut::<4>().0.iter_mut() {
        let n = 1.0 + noise();
        for v in &mut p[..3] {
            *v *= n;
        }
    }
    let a = answer_for(&grainy);
    assert!(has(&a, LookTag::MoreGrain), "{:?}", a.tags);
    assert!(a.grade.iter().any(|s| s.see == "color.finishing.grain"));
    assert_grounded(&a);
    // the same picture: only the Shot Match step, no warning
    let a = answer_for(&neutral());
    assert!(a.tags.is_empty() && a.grade.len() == 1 && a.too_different.is_none(), "{a:?}");
    assert_grounded(&a);
}

#[test]
fn a_night_sky_fading_to_black_is_not_a_letterbox() {
    // dark at the top and bottom edges, but no edge where a bar would end
    let night = map(&neutral(), |c, _, y| {
        let d = (y.min(179 - y) as f32 / 40.0).min(1.0);
        c.map(|v| v * 0.3 * d * d)
    });
    let f = facts(&night);
    assert_eq!(f.letterbox, None, "{f:?}");
    let a = recipe(&f, &facts(&neutral()), 1, 2);
    assert!(!a.tags.contains(&LookTag::Letterbox), "{:?}", a.tags);
}

#[test]
fn a_grey_shot_against_a_neon_reference_is_too_different() {
    let grey = map(&neutral(), |c, _, _| {
        let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        [l; 3]
    });
    let neon = map(&neutral(), |c, _, _| [c[0] * 1.6, c[1] * 0.3, c[2] * 1.8]);
    let a = recipe(&facts(&neon), &facts(&grey), 1, 2);
    assert!(a.too_different.is_some() || a.tags.contains(&LookTag::MoreSaturated), "{a:?}");
}

#[test]
fn the_prompt_carries_the_recipe_and_its_sections() {
    let faded = map(&neutral(), |c, _, _| c.map(|v| 0.06 + 0.94 * v));
    let r = std::sync::Arc::new(answer_for(&faded));
    let ctx = crate::coach_assist::AssistContext { reference: Some(r.clone()), ..Default::default() };
    let p = crate::coach_assist::assistant_prompt("how do I get this look?", &ctx, crate::coach_assist::DEFAULT_BUDGET_CHARS);
    // (the recipe itself takes part of the budget: its first section always goes, the rest as fit,
    // ahead of anything retrieval found; every step's text and section id are in the recipe)
    assert_eq!(p.sources.first().map(String::as_str), Some(r.grade[0].see), "{:?}", p.sources);
    for (i, s) in p.sources.iter().enumerate() {
        let pinned = r.grade.iter().position(|st| st.see == s);
        assert!(pinned.is_some() || p.sources.iter().skip(i).all(|x| !r.grade.iter().any(|st| st.see == x)), "recipe sections come first: {:?}", p.sources);
    }
    for st in &r.grade {
        assert!(p.user.contains(st.text) && p.user.contains(&format!("[{}]", st.see)), "{} missing", st.see);
    }
    assert!(p.user.contains("In the grade") && p.user.contains("On set"), "{}", p.user);
    assert!(p.user.contains("Reference picture vs this shot (measured)"), "{}", p.user);
}

#[test]
fn night_against_day_says_a_grade_only_gets_part_of_the_way() {
    let night = map(&neutral(), |c, _, _| c.map(|v| v / 16.0));
    let a = answer_for(&night);
    let note = a.too_different.clone().expect("too different");
    assert!(note.contains("stops") && note.contains("darker"), "{note}");
    assert!(a.text().contains("Note: "), "{}", a.text());
    assert_grounded(&a);
}

fn reference_png(tag: &str, f: impl Fn(u32, u32) -> [u8; 4]) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("fc-ref-{tag}-{}-{n:x}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{tag}.png"));
    image::RgbaImage::from_fn(480, 270, |x, y| image::Rgba(f(x, y))).save(&p).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn coach_reference_command_measures_answers_and_its_do_this_runs() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let path = reference_png("look", |x, y| if !(34..236).contains(&y) { [0, 0, 0, 255] } else { [40 + (x % 120) as u8, 70, 90, 255] });
    let still = s.execute("stills.import", json!({"path": path})).unwrap()["still"].as_u64().unwrap();
    let r = s.execute("coach.reference", json!({"still": still})).unwrap();
    assert!(r["tags"].as_array().is_some_and(|t| t.contains(&json!("Letterbox"))), "{r}");
    assert!(r["text"].as_str().is_some_and(|t| t.contains("In the grade:") && t.contains("On set:")), "{r}");
    assert!(!r["measured"].as_array().unwrap().is_empty(), "{r}");
    // the Do this action is an ordinary command: one Shot Match node
    let d = &r["doThis"];
    assert_eq!(d["command"], json!("grade.shotMatch"));
    let clip = d["params"]["clip"].as_u64().unwrap();
    s.execute("grade.shotMatch", d["params"].clone()).unwrap();
    let q = s.active_sequence().unwrap();
    let g = q.find_item(filmcraft_project::ClipId(clip)).unwrap().1.grade.clone().unwrap();
    assert_eq!(g.nodes.last().map(|n| n.label.as_str()), Some("Shot Match"));
    // hostile params
    for p in [json!({}), json!({"still": "x"}), json!({"still": 987_654_321u64}), json!({"still": still, "clip": 987_654_321u64})] {
        assert!(s.execute("coach.reference", p.clone()).is_err(), "{p}");
    }
}

#[test]
fn a_question_about_a_look_picks_the_compared_reference_picture() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let path = reference_png("pick", |_, _| [120, 90, 60, 255]);
    let still = s.execute("stills.import", json!({"path": path})).unwrap()["still"].as_u64().unwrap();
    let pick = |s: &Session, q: &str, auto: bool| crate::coach_reference::reference_for(s, &json!({}), q, auto).unwrap();
    assert_eq!(pick(&s, "how do I get this look?", true), None, "nothing compared yet");
    s.execute("compare.set", json!({"still": still, "mode": "wipe"})).unwrap();
    assert_eq!(pick(&s, "How do I get this look?", true), Some(still));
    assert_eq!(pick(&s, "how do I add a marker?", true), None, "not about a look");
    assert_eq!(pick(&s, "why does the face look green?", true), None, "\"look\" alone is not about the reference");
    assert_eq!(pick(&s, "how do I match these two clips?", true), None, "matching clips is not about the reference");
    assert_eq!(pick(&s, "can I make my shot like this picture?", true), Some(still));
    assert_eq!(pick(&s, "how do I get this look?", false), None, "no auto context");
    // a grabbed still is not a reference picture
    let grabbed = s.execute("stills.grab", json!({})).unwrap()["still"].as_u64().unwrap();
    s.execute("compare.set", json!({"still": grabbed})).unwrap();
    assert_eq!(pick(&s, "how do I get this look?", true), None);
    // explicit, and hostile
    assert_eq!(crate::coach_reference::reference_for(&s, &json!({"reference": still}), "anything", false).unwrap(), Some(still));
    assert!(crate::coach_reference::reference_for(&s, &json!({"reference": "x"}), "q", true).is_err());
    assert!(crate::coach_reference::reference_for(&s, &json!({"reference": 987_654_321u64}), "q", true).is_err());
}

struct Fixed(&'static str);

impl crate::ai_explain::ExplainBackend for Fixed {
    fn ready(&self, _: &filmcraft_llm::ModelInfo) -> std::result::Result<(), (crate::ai_explain::Fallback, String)> {
        Ok(())
    }
    fn generate(&self, _: &filmcraft_llm::ModelInfo, _: &str, _: &str, _: filmcraft_llm::GenOptions) -> std::result::Result<String, String> {
        Ok(self.0.into())
    }
}

struct Looker(std::sync::Mutex<Vec<String>>);

impl crate::coach_assist::PictureLooker for Looker {
    fn look(&self, _: &crate::coach_assist::PictureInput, question: &str) -> Result<String, String> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(question.to_string());
        Ok("Soft window light from the left, a blurred background.".into())
    }
}

#[test]
fn the_assistant_answers_a_look_question_with_the_reference_and_keeps_it_private() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let path = reference_png("secret-moodboard-okafor", |x, _| [200, 120 + (x % 40) as u8, 80, 255]);
    let still = s.execute("stills.import", json!({"path": path})).unwrap()["still"].as_u64().unwrap();
    s.execute("compare.set", json!({"still": still, "mode": "wipe"})).unwrap();
    s.assist.backend = Some(std::sync::Arc::new(Fixed("Give each node one job, then adjust each on its own.")));
    let looker = std::sync::Arc::new(Looker(Default::default()));
    s.assist.looker = Some(looker.clone());
    let id = s.execute("coach.assist", json!({"question": "how do I get this look?", "context": "auto", "style": "free"})).unwrap()["id"].as_u64().unwrap();
    let r = s.execute("coach.assistResult", json!({"id": id, "wait": true})).unwrap();
    assert_eq!(r["state"], json!("done"), "{r}");
    let reference = &r["reference"];
    assert_eq!(reference["still"], json!(still), "{r}");
    assert!(reference["grade"].as_array().is_some_and(|g| !g.is_empty()) && reference["onSet"].as_array().is_some_and(|g| !g.is_empty()), "{r}");
    assert_eq!(reference["doThis"]["command"], json!("grade.shotMatch"));
    assert_eq!(reference["seen"], json!("Soft window light from the left, a blurred background."), "{r}");
    // the vision model was asked about the reference with the reference prompt
    let asked = looker.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert!(asked.iter().any(|q| q.contains("reference picture") && q.contains("depth of field")), "{asked:?}");
    // a shared example never carries the reference: not its name, its description or its numbers
    let record = s.assist.records.get(id).unwrap();
    let a = s.assist.result(id).flatten();
    let all = crate::coach_share::Extras { grade: true, picture: true, frame: true };
    let text = crate::coach_share::example_text(&record, a.as_ref(), all, false, &crate::coach_share::Private::default());
    for leak in ["secret-moodboard", "okafor", "Soft window light", "Theirs vs yours", "In the grade:"] {
        assert!(!text.to_lowercase().contains(&leak.to_lowercase()), "`{leak}` leaked:\n{text}");
    }
    // nor the training inbox's copy of the answer
    let inbox = a.as_ref().map(|a| a.shareable_json().to_string()).unwrap_or_default();
    assert!(inbox.contains("privateReference"), "{inbox}");
    for leak in ["Soft window light", "\"reference\"", "onSet", "measured"] {
        assert!(!inbox.contains(leak), "`{leak}` in the inbox copy: {inbox}");
    }
    // the still's name is redacted from anything shared
    let private = crate::coach_share::Private::of(&s);
    assert!(!private.strip("about secret-moodboard-okafor").contains("secret-moodboard"), "{}", private.strip("about secret-moodboard-okafor"));
    // without a vision model it still answers from the numbers
    s.assist.looker = None;
    let id = s.execute("coach.assist", json!({"question": "how do I get this look?", "context": "auto", "reference": still})).unwrap()["id"].as_u64().unwrap();
    let r = s.execute("coach.assistResult", json!({"id": id, "wait": true})).unwrap();
    assert!(r["reference"]["grade"].as_array().is_some_and(|g| !g.is_empty()), "{r}");
}

/// REF on the real models (feature `llm`; the assistant model and the Qwen3-VL release in the
/// models folder): one "how do I get this look?" answer about a teal / orange reference, printed.
/// `FILMCRAFT_LLM_DIR=<models folder> cargo test --release -p filmcraft-engine --features llm --lib
/// real_reference_answer -- --ignored --nocapture`.
#[cfg(feature = "llm")]
#[test]
#[ignore = "needs the assistant and picture models (several GB) and a minute of CPU / GPU"]
fn real_reference_answer() {
    let Some(dir) = std::env::var_os("FILMCRAFT_LLM_DIR") else { return };
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.assist.backend = Some(std::sync::Arc::new(crate::ai_explain::LlmBackend::with_dir(std::path::PathBuf::from(dir))));
    let path = reference_png("real-teal-orange", |x, y| {
        let warm = (150..330).contains(&x) && (60..240).contains(&y);
        if !(30..240).contains(&y) {
            [0, 0, 0, 255]
        } else if warm {
            [235, 160, 105, 255]
        } else {
            [25, 75, 85, 255]
        }
    });
    let still = s.execute("stills.import", json!({"path": path})).unwrap()["still"].as_u64().unwrap();
    let t = std::time::Instant::now();
    let id = s.execute("coach.assist", json!({"question": "how do I get this look?", "context": "auto", "reference": still, "style": "free"})).unwrap()["id"]
        .as_u64()
        .unwrap();
    let r = s.execute("coach.assistResult", json!({"id": id, "wait": true})).unwrap();
    eprintln!("{:.1} s\n{}", t.elapsed().as_secs_f32(), serde_json::to_string_pretty(&r).unwrap_or_default());
    assert_eq!(r["state"], json!("done"));
}
