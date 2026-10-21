//! Training data for the assistant tune (fork FORK-PLAN §1a.97 item 2, AI-PLAN AI4 item 5 (b),
//! design note of owner question Q46).
//!
//! ```text
//! cargo run --release -p filmcraft-engine --example assist_traindata -- \
//!     --out-dir ~/Apps/filmcraft-train/runs/assist-data-v1 [--pool docs/fork/coach-eval/pool] [--val 0.1] [--seed 1]
//! ```
//!
//! For every training pair in the pool (`*.jsonl`: `{q, a, sources, kind, family?}`), the prompt is
//! the app's own Pick prompt ([`coach_assist::pick_prompt`], the default style, no on-screen
//! context) rendered with SmolLM3's chat template ([`filmcraft_llm::chat_prompt`]), and the target
//! is the keys of the offered steps that come from the pair's sources: the steps best matching the
//! pair's answer (shared words), at most [`coach_assist::PICK_MAX`], in the order the answer does
//! them. A pair none of whose sources the search offered is skipped (the tune must not learn to
//! answer nothing for a search miss), and so is a pair whose question is also a held-out question
//! (`*.toml`). Pairs are split into train / val by family (a family's pairs stay together), so
//! val measures families the tune hasn't seen.
//!
//! Writes `train.jsonl` / `val.jsonl` (`{"id", "family", "messages": [system, user, assistant],
//! "prompt"}`; `tools/explainer-train/train.py` uses `prompt` as the exact prompt text) and
//! `data-stats.json`. A developer tool: errors end the run with a message.

use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};

use filmcraft_engine::coach_assist::{self, AssistContext, AssistStyle, PICK_MAX, PickStep};
use serde_json::{Value, json};

/// Words of three letters or more, lower-cased.
fn words(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).map(str::to_lowercase).collect()
}

/// The answer cut into the parts it does in order: numbered steps ("1. … 2. …") or sentences.
fn answer_parts(a: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = a.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        // a new numbered step: a digit run followed by ". " at the start or after a space
        let starts_step = c.is_ascii_digit()
            && (i == 0 || chars.get(i.wrapping_sub(1)).is_some_and(|p| p.is_whitespace()))
            && chars.get(i + 1..).is_some_and(|r| r.iter().skip_while(|x| x.is_ascii_digit()).take(2).collect::<String>() == ". ");
        if starts_step && !cur.trim().is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        cur.push(*c);
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    if parts.len() == 1 {
        // no numbered steps: sentences
        return a.split_inclusive(['.', '!', '?']).map(str::to_string).filter(|s| !s.trim().is_empty()).collect();
    }
    parts
}

/// The target keys: offered steps from `sources` that share words with an answer part, best
/// overlap first (at most [`PICK_MAX`]), then put in the answer's order.
fn target_keys(steps: &[PickStep], sources: &[String], answer: &str) -> Vec<String> {
    let parts: Vec<HashSet<String>> = answer_parts(answer).iter().map(|p| words(p)).collect();
    let mut scored: Vec<(f64, usize, &PickStep)> = Vec::new();
    for s in steps.iter().filter(|s| sources.contains(&s.source)) {
        let sw = words(&s.text);
        if sw.is_empty() {
            continue;
        }
        let best = parts
            .iter()
            .enumerate()
            .map(|(i, p)| (sw.intersection(p).count() as f64 / sw.len().min(p.len().max(1)) as f64, i))
            .max_by(|a, b| a.0.total_cmp(&b.0).then(b.1.cmp(&a.1)));
        if let Some((score, at)) = best.filter(|(score, _)| *score >= 0.25) {
            scored.push((score, at, s));
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(PICK_MAX);
    scored.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.key.cmp(&b.2.key)));
    scored.into_iter().map(|(_, _, s)| s.key.clone()).collect()
}

/// FNV-1a, for a stable family → split choice.
fn fnv(s: &str, seed: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn norm(q: &str) -> String {
    q.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn held_out_questions(pool: &Path) -> Result<HashSet<String>, Box<dyn Error>> {
    let mut out = HashSet::new();
    for e in std::fs::read_dir(pool)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "toml") {
            let v: toml::Value = toml::from_str(&std::fs::read_to_string(&p)?)?;
            for q in v.get("question").and_then(toml::Value::as_array).into_iter().flatten() {
                if let Some(text) = q.get("q").and_then(toml::Value::as_str) {
                    out.insert(norm(text));
                }
            }
        }
    }
    Ok(out)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let out_dir = PathBuf::from(arg("--out-dir").ok_or("--out-dir is required")?);
    let pool = PathBuf::from(arg("--pool").unwrap_or_else(|| "docs/fork/coach-eval/pool".into()));
    let val_share: f64 = arg("--val").map_or(Ok(0.1), |v| v.parse())?;
    let seed: u64 = arg("--seed").map_or(Ok(1), |v| v.parse())?;
    let held = held_out_questions(&pool)?;
    let mut files: Vec<PathBuf> =
        std::fs::read_dir(&pool)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect();
    files.sort();
    let ctx = AssistContext { reference: None, reference_picture: None, reference_seen: None, style: AssistStyle::Pick, ..Default::default() };
    let (mut train, mut val) = (Vec::new(), Vec::new());
    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    let mut target_lens: BTreeMap<usize, usize> = BTreeMap::new();
    for f in &files {
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("pool").to_string();
        for (n, line) in std::fs::read_to_string(f)?.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let pair: Value = serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", f.display(), n + 1))?;
            let (Some(q), Some(a)) = (pair.get("q").and_then(Value::as_str), pair.get("a").and_then(Value::as_str)) else {
                *skipped.entry("malformed").or_default() += 1;
                continue;
            };
            if held.contains(&norm(q)) {
                *skipped.entry("also held out").or_default() += 1;
                continue;
            }
            let sources: Vec<String> =
                pair.get("sources").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            let family = pair.get("family").and_then(Value::as_str).map_or_else(|| format!("{stem}#{n}"), str::to_string);
            let p = coach_assist::pick_prompt(q, &ctx, coach_assist::DEFAULT_BUDGET_CHARS);
            if !p.steps.iter().any(|s| sources.contains(&s.source)) {
                *skipped.entry("sources not retrieved").or_default() += 1;
                continue;
            }
            let keys = target_keys(&p.steps, &sources, a);
            if keys.is_empty() {
                *skipped.entry("no step matches the answer").or_default() += 1;
                continue;
            }
            *target_lens.entry(keys.len()).or_default() += 1;
            let answer = keys.join(" ");
            let prompt = filmcraft_llm::chat_prompt(filmcraft_llm::Family::SmolLm3, &p.prompt.system, &p.prompt.user);
            let row = json!({
                "id": format!("{stem}#{}", n + 1),
                "family": family,
                "messages": [
                    {"role": "system", "content": p.prompt.system},
                    {"role": "user", "content": p.prompt.user},
                    {"role": "assistant", "content": answer},
                ],
                "prompt": prompt,
            });
            if (fnv(&family, seed) % 1000) as f64 / 1000.0 < val_share { val.push(row) } else { train.push(row) }
        }
    }
    std::fs::create_dir_all(&out_dir)?;
    let write = |name: &str, rows: &[Value]| -> std::io::Result<()> {
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(out_dir.join(name), text)
    };
    write("train.jsonl", &train)?;
    write("val.jsonl", &val)?;
    let stats = json!({"seed": seed, "train": train.len(), "val": val.len(), "skipped": skipped, "target_keys": target_lens, "held_out_questions": held.len()});
    std::fs::write(out_dir.join("data-stats.json"), serde_json::to_string_pretty(&stats)?)?;
    println!("{stats}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_answers_split_into_steps() {
        assert_eq!(answer_parts("1. Add a node. 2. Pick the car. 10. Done").len(), 3);
        assert_eq!(answer_parts("Open it. Then grade it!").len(), 2);
        assert!(answer_parts("").is_empty());
    }
}
