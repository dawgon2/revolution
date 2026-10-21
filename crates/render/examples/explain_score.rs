//! Score AI-written node explanations against the fact checker (GRADE-PLAN G2.11b).
//!
//! ```text
//! cargo run --release -p filmcraft-render --example explain_score -- outputs.jsonl [more.jsonl ...]
//!     [--summary summary.md] [--json summary.json] [--fails]
//! cargo run --release -p filmcraft-render --example explain_score -- --dump-cases cases.jsonl
//!     [--set 1|2|3|5|all] [--cases set4.jsonl] [--dedupe train.jsonl [--dedupe val.jsonl ...]]
//! ```
//!
//! `--dump-cases` writes the held-out cases as JSON lines `{label, facts, template}` (the input
//! of `filmcraft-llm`'s `llm_eval`) and stops. `--set` picks the set: `1` (default) the 36 cases
//! of [`eval_cases`], `2` the second set of [`eval_cases_v2`] (another frame and seed, labels
//! `h2_…`), `3` the third ([`eval_cases_v3`], `h3_…`), `5` the branch set of
//! [`branch_eval_cases`] (fork AI2: notes of parallel / layer / splitter / combiner / key-mixer /
//! outside nodes, labels `b5_…`, checked with `check_branch_explanation`), `all` sets 1–3.
//! `--cases <jsonl>` scores (or dedupes) a set dumped elsewhere instead, as lines `{label, facts,
//! template, accept}` checked with `check_phrasings(accept, text)`: set 4, the comp lines
//! (`filmcraft-engine`'s `comp_traindata --dump-cases`, labels `c4_…`). `--dedupe` reads training chat files (`explain_traindata`'s
//! `train.jsonl` / `val.jsonl`) and drops every case whose compact facts (as the prompt carries
//! them) equal a training example's, as the generator drops training grades equal to set 1. It
//! also reports near-duplicates: kept cases whose reference sentence (without its "Node k" label)
//! equals a training example's, i.e. facts that differ only below the precision the sentence and
//! the checker word them in.
//!
//! When scoring, `--set` limits the cases answers are scored on (answers for other labels are
//! counted as unknown and left out of the pass rate).
//!
//! Each input line is one answer as written by `filmcraft-llm`'s `llm_eval --out`:
//! `{"model", "label", "text", "prefill_ms", "decode_ms", "stop", ...}`. The label names one of the
//! held-out cases of the selected set (`--set`); the text is checked
//! with `check_explanation_labeled(facts, None, Some(1), text)` (the eval prompt labels the node
//! "Node 1"). Per model: pass rate, the histogram of rejection reasons (the `FactMismatch`
//! variant), mean time per answer (prefill + decode) and mean answer length. Unknown labels and
//! unreadable lines are counted, never fatal.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::Write as _;

use filmcraft_render::grade_explain::cases::{EvalCase, branch_eval_cases, eval_cases, eval_cases_v2, eval_cases_v3};
use filmcraft_render::grade_explain::{BranchFacts, FactMismatch, NodeFacts, check_branch_explanation, check_explanation_labeled, check_phrasings};

/// The facts a held-out case is checked against.
enum Facts {
    Node(Box<NodeFacts>),
    Branch(BranchFacts),
    /// A set dumped elsewhere: its facts as given and the phrasings the checker accepts.
    Fixed {
        facts: serde_json::Value,
        accept: Vec<String>,
    },
}

/// One held-out case of any set.
struct Held {
    label: String,
    facts: Facts,
    template: String,
}

impl Held {
    fn facts_json(&self) -> Result<String, serde_json::Error> {
        match &self.facts {
            Facts::Node(f) => serde_json::to_string(f),
            Facts::Branch(f) => serde_json::to_string(f),
            Facts::Fixed { facts, .. } => serde_json::to_string(facts),
        }
    }

    /// The line `llm_eval` reads: `{label, facts, template}`.
    fn line(&self) -> Option<String> {
        let facts: serde_json::Value = serde_json::from_str(&self.facts_json().ok()?).ok()?;
        let mut v = serde_json::json!({"label": self.label, "facts": facts, "template": self.template});
        if let (Facts::Fixed { accept, .. }, Some(o)) = (&self.facts, v.as_object_mut()) {
            o.insert("accept".into(), serde_json::json!(accept));
        }
        Some(v.to_string())
    }

    /// The node prompt labels the node "Node 1"; a branch note names its nodes itself.
    fn check(&self, answer: &str) -> Result<(), FactMismatch> {
        match &self.facts {
            Facts::Node(f) => check_explanation_labeled(f, None, Some(1), answer),
            Facts::Branch(f) => check_branch_explanation(f, answer),
            Facts::Fixed { accept, .. } => check_phrasings(accept, answer),
        }
    }
}

fn node_held(c: EvalCase) -> Held {
    Held { label: c.label, facts: Facts::Node(Box::new(c.facts)), template: c.template }
}

#[derive(Default)]
struct ModelScore {
    answers: usize,
    passed: usize,
    reasons: BTreeMap<String, usize>,
    total_ms: f64,
    timed: usize,
    gen_tokens: usize,
    unknown_labels: usize,
    passed_labels: Vec<String>,
    fails: Vec<(String, String, String)>,
}

/// The variant name of a rejection (`Ungrammatical`, `Direction`, ...).
fn reason_name(e: &FactMismatch) -> String {
    let dbg = format!("{e:?}");
    dbg.split(['(', ' ', '{']).next().unwrap_or("?").to_string()
}

fn num(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(serde_json::Value::as_f64).filter(|x| x.is_finite() && *x >= 0.0)
}

struct Args {
    inputs: Vec<String>,
    summary: Option<String>,
    json: Option<String>,
    fails: bool,
    dump: Option<String>,
    set: String,
    dedupe: Vec<String>,
    cases: Option<String>,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut a = Args { inputs: Vec::new(), summary: None, json: None, fails: false, dump: None, set: "1".into(), dedupe: Vec::new(), cases: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--summary" => a.summary = Some(it.next().ok_or("--summary needs a path")?),
            "--json" => a.json = Some(it.next().ok_or("--json needs a path")?),
            "--fails" => a.fails = true,
            "--dump-cases" => a.dump = Some(it.next().ok_or("--dump-cases needs a path")?),
            "--set" => a.set = it.next().ok_or("--set needs 1, 2, 3, 5 or all")?,
            "--dedupe" => a.dedupe.push(it.next().ok_or("--dedupe needs a path")?),
            "--cases" => a.cases = Some(it.next().ok_or("--cases needs a path")?),
            _ => a.inputs.push(arg),
        }
    }
    if !matches!(a.set.as_str(), "1" | "2" | "3" | "5" | "all") {
        return Err(format!("--set {}: expected 1, 2, 3, 5 or all", a.set).into());
    }
    if a.inputs.is_empty() && a.dump.is_none() {
        return Err("usage: explain_score <outputs.jsonl>... [--summary out.md] [--json out.json] [--fails]".into());
    }
    Ok(a)
}

fn selected_cases(set: &str) -> Vec<Held> {
    let node = match set {
        "2" => eval_cases_v2(),
        "3" => eval_cases_v3(),
        "5" => Vec::new(),
        "all" => eval_cases().into_iter().chain(eval_cases_v2()).chain(eval_cases_v3()).collect(),
        _ => eval_cases(),
    };
    let mut v: Vec<Held> = node.into_iter().map(node_held).collect();
    if set == "5" {
        v.extend(branch_eval_cases().into_iter().map(|c| Held { label: c.label, facts: Facts::Branch(c.facts), template: c.template }));
    }
    v
}

/// The compact facts and the reference sentence (without its "Node k …:" label) of a prompt's user
/// text, as `filmcraft_llm::explain_prompt` writes it.
fn prompt_parts(user: &str) -> Option<(String, String)> {
    let rest = user.strip_prefix("Facts (JSON):\n")?;
    let (facts, rest) = rest.split_once("\n\nReference sentence:\n")?;
    let template = rest.split_once("\n\nExplain").map_or(rest, |(t, _)| t);
    Some((facts.to_string(), strip_label(template)))
}

fn strip_label(template: &str) -> String {
    let t = template.trim();
    match t.split_once(": ") {
        Some((head, body)) if head.starts_with("Node ") => body.to_string(),
        _ => t.to_string(),
    }
}

/// Drop the cases whose compact facts equal a training example's (see the module docs); prints the
/// counts and the near-duplicates.
fn dedupe(cases: Vec<Held>, files: &[String]) -> Result<Vec<Held>, Box<dyn Error>> {
    let (mut facts, mut sentences, mut targets) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut examples = 0usize;
    for path in files {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) else { continue };
            let content =
                |role: &str| msgs.iter().find(|m| m.get("role").and_then(|r| r.as_str()) == Some(role)).and_then(|m| m.get("content")).and_then(|c| c.as_str());
            let Some((f, s)) = content("user").and_then(prompt_parts) else { continue };
            examples += 1;
            facts.insert(f);
            sentences.insert(s);
            if let Some(t) = content("assistant") {
                targets.insert(strip_label(t));
            }
        }
    }
    let (mut kept, mut exact, mut near, mut as_target) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for c in cases {
        let facts_json = c.facts_json()?;
        let (_, user) = filmcraft_llm::explain_prompt(&facts_json, &c.template);
        let Some((f, s)) = prompt_parts(&user) else { continue };
        if facts.contains(&f) {
            exact.push(c.label);
            continue;
        }
        if sentences.contains(&s) {
            near.push(c.label.clone());
        }
        if targets.contains(&s) {
            as_target.push(c.label.clone());
        }
        kept.push(c);
    }
    println!("dedupe against {examples} training examples ({} distinct facts)", facts.len());
    println!("dropped {} with exactly equal compact facts: {}", exact.len(), exact.join(", "));
    println!("kept {}; near-duplicates (same reference sentence as a training example): {}: {}", kept.len(), near.len(), near.join(", "));
    println!("kept cases whose reference sentence is also a training target: {}: {}", as_target.len(), as_target.join(", "));
    Ok(kept)
}

/// The cases of a set dumped elsewhere (`--cases`): unreadable lines are skipped and counted.
fn cases_file(path: &str) -> Result<Vec<Held>, Box<dyn Error>> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let (mut v, mut bad) = (Vec::new(), 0usize);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(c) = serde_json::from_str::<serde_json::Value>(line) else {
            bad += 1;
            continue;
        };
        let (Some(label), Some(template), Some(facts), Some(accept)) =
            (c.get("label").and_then(|x| x.as_str()), c.get("template").and_then(|x| x.as_str()), c.get("facts"), c.get("accept").and_then(|x| x.as_array()))
        else {
            bad += 1;
            continue;
        };
        let accept: Vec<String> = accept.iter().filter_map(|a| a.as_str().map(str::to_string)).collect();
        v.push(Held { label: label.to_string(), facts: Facts::Fixed { facts: facts.clone(), accept }, template: template.to_string() });
    }
    if bad > 0 {
        eprintln!("{path}: skipped {bad} unreadable case line(s)");
    }
    Ok(v)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    if let Some(p) = &args.dump {
        let mut cases = match &args.cases {
            Some(p) => cases_file(p)?,
            None => selected_cases(&args.set),
        };
        if !args.dedupe.is_empty() {
            cases = dedupe(cases, &args.dedupe)?;
        }
        let lines: Vec<String> = cases.iter().filter_map(Held::line).collect();
        std::fs::write(p, lines.join("\n") + "\n").map_err(|e| format!("{p}: {e}"))?;
        println!("wrote {} cases to {p}", lines.len());
        return Ok(());
    }
    let all = match &args.cases {
        Some(p) => cases_file(p)?,
        None => selected_cases(&args.set),
    };
    let cases: BTreeMap<String, Held> = all.into_iter().map(|c| (c.label.clone(), c)).collect();
    let mut models: BTreeMap<String, ModelScore> = BTreeMap::new();
    let mut bad_lines = 0usize;
    for path in &args.inputs {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                bad_lines += 1;
                continue;
            };
            let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("?").to_string();
            let label = v.get("label").and_then(|m| m.as_str()).unwrap_or("?").to_string();
            let answer = v.get("text").and_then(|m| m.as_str()).unwrap_or("").to_string();
            let s = models.entry(model).or_default();
            let Some(case) = cases.get(&label) else {
                s.unknown_labels += 1;
                continue;
            };
            s.answers += 1;
            if let (Some(p), Some(d)) = (num(&v, "prefill_ms"), num(&v, "decode_ms")) {
                s.total_ms += p + d;
                s.timed += 1;
            }
            s.gen_tokens += num(&v, "gen_tokens").map_or(0, |t| t as usize);
            match case.check(&answer) {
                Ok(()) => {
                    s.passed += 1;
                    s.passed_labels.push(label);
                }
                Err(e) => {
                    *s.reasons.entry(reason_name(&e)).or_default() += 1;
                    s.fails.push((label, e.to_string(), answer));
                }
            }
        }
    }

    let mut md = String::new();
    let _ = writeln!(md, "| model | pass | rate | mean s | mean tokens | rejection reasons |");
    let _ = writeln!(md, "|---|---|---|---|---|---|");
    let mut json = serde_json::Map::new();
    for (name, s) in &models {
        let rate = if s.answers > 0 { s.passed as f64 / s.answers as f64 } else { 0.0 };
        let mean_s = if s.timed > 0 { s.total_ms / s.timed as f64 / 1000.0 } else { f64::NAN };
        let mean_tok = if s.answers > 0 { s.gen_tokens as f64 / s.answers as f64 } else { 0.0 };
        let reasons: Vec<String> = {
            let mut r: Vec<(&String, &usize)> = s.reasons.iter().collect();
            r.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            r.iter().map(|(k, v)| format!("{k} {v}")).collect()
        };
        let _ = writeln!(
            md,
            "| {name} | {}/{} | {:.0} % | {mean_s:.2} | {mean_tok:.0} | {} |",
            s.passed,
            s.answers,
            rate * 100.0,
            if reasons.is_empty() { "-".to_string() } else { reasons.join(", ") }
        );
        json.insert(
            name.clone(),
            serde_json::json!({
                "answers": s.answers,
                "passed": s.passed,
                "pass_rate": rate,
                "mean_s": if mean_s.is_finite() { serde_json::json!(mean_s) } else { serde_json::Value::Null },
                "mean_gen_tokens": mean_tok,
                "reasons": s.reasons,
                "passed_labels": s.passed_labels,
                "unknown_labels": s.unknown_labels,
            }),
        );
    }
    if bad_lines > 0 {
        let _ = writeln!(md, "\n{bad_lines} unreadable line(s) skipped.");
    }
    if args.fails {
        for (name, s) in &models {
            let _ = writeln!(md, "\n## {name}: rejected answers\n");
            for (label, why, text) in &s.fails {
                let _ = writeln!(md, "- `{label}`: {why}\n  > {}", text.replace('\n', " "));
            }
        }
    }
    print!("{md}");
    if let Some(p) = &args.summary {
        std::fs::write(p, &md).map_err(|e| format!("{p}: {e}"))?;
    }
    if let Some(p) = &args.json {
        std::fs::write(p, serde_json::to_string_pretty(&serde_json::Value::Object(json))?).map_err(|e| format!("{p}: {e}"))?;
    }
    Ok(())
}
