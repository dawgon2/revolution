//! Training data for the explainer's comp-graph lines (fork AI2 item 1).
//!
//! ```text
//! cargo run --release -p filmcraft-engine --example comp_traindata -- \
//!     --out-dir ~/Apps/filmcraft-train/runs/<name> [--comps 1500] [--per-node 2] [--seed 1] [--val 0.05]
//! cargo run --release -p filmcraft-engine --example comp_traindata -- --dump-cases set4.jsonl
//! ```
//!
//! Builds random comps (Media In from layers or a missing clip, now and then late; Merge, Transform,
//! Key, Mask, Blur, Color, Solid, Gradient; bypassed and unconnected nodes; parameters off their
//! defaults, near-default ones too), reads each node's [`CompFacts`] and writes chat examples in
//! `explain_traindata`'s format: system + user = `filmcraft_llm::explain_prompt(facts, template)`,
//! assistant = one of the line's fixed phrasings ([`CompFacts::phrasings`]), each checked with
//! [`check_comp_explanation`]. Facts equal to a held-out set-4 case ([`comp_eval_cases`]) are
//! dropped. Appends to `train.jsonl` / `val.jsonl` in `--out-dir` (so it adds to a folder
//! `explain_traindata` wrote) and writes `comp-data-stats.json`. Deterministic for a seed.
//!
//! `--dump-cases` writes held-out set 4 as JSON lines `{label, facts, template, accept}` for
//! `llm_eval` and `explain_score --cases` (`accept`: the phrasings the checker takes) and stops.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::io::Write as _;

use filmcraft_engine::comp_explain::{CompFacts, check_comp_explanation, comp_eval_cases, comp_facts, empty_comp, float_params, link, new_node, set_float};
use filmcraft_project::Project;
use filmcraft_project::comp::{CompKind, KEY_EFFECTS, MediaInSource};
use filmcraft_project::grade::GradeNode;
use filmcraft_time::{TICKS_PER_SECOND, Tick};

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn chance(&mut self, p: f64) -> bool {
        self.f() < p
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
    fn word(&mut self, xs: &[&'static str]) -> &'static str {
        xs.get(self.below(xs.len())).copied().unwrap_or("")
    }
}

const NAMES: &[&str] = &["Plate", "Actor", "Backdrop", "Sky fill", "Logo", "Push in", "Soften", "Green screen", "Vignette", "Warm", "Title", "Comp"];

struct Args {
    out_dir: std::path::PathBuf,
    comps: usize,
    per_node: usize,
    seed: u64,
    val: f64,
    dump: Option<String>,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut a = Args { out_dir: std::path::PathBuf::new(), comps: 1500, per_node: 2, seed: 1, val: 0.05, dump: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut v = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--out-dir" => a.out_dir = v()?.into(),
            "--comps" => a.comps = v()?.parse::<usize>()?.min(1_000_000),
            "--per-node" => a.per_node = v()?.parse::<usize>()?.clamp(1, 4),
            "--seed" => a.seed = v()?.parse()?,
            "--val" => a.val = v()?.parse::<f64>()?.clamp(0.0, 0.5),
            "--dump-cases" => a.dump = Some(v()?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if a.out_dir.as_os_str().is_empty() && a.dump.is_none() {
        return Err("usage: comp_traindata --out-dir <dir> [--comps N] [--per-node K] [--seed S] [--val F] | --dump-cases <path>".into());
    }
    Ok(a)
}

/// A random label (or none, so the node is named "Kind id").
fn label(rng: &mut Rng) -> &'static str {
    if rng.chance(0.4) { rng.word(NAMES) } else { "" }
}

/// A processing node `id` of a random kind with random parameters.
fn random_node(rng: &mut Rng, id: u64) -> filmcraft_project::comp::CompNode {
    let kind = match rng.below(6) {
        0 => CompKind::Transform,
        1 => CompKind::Key { effect: rng.word(KEY_EFFECTS).to_string() },
        2 => CompKind::Mask { masks: Vec::new() },
        3 => CompKind::Blur,
        4 => CompKind::Color { node: GradeNode::new(1) },
        _ => CompKind::Transform,
    };
    let mut n = new_node(id, kind, label(rng));
    let floats = float_params(&n.kind).len();
    // up to three parameters off their defaults; now and then just off (near-default values)
    for _ in 0..rng.below(4) {
        let which = rng.below(floats.max(1));
        let frac = if rng.chance(0.2) { 0.5 + (rng.f() - 0.5) * 0.02 } else { rng.f() };
        set_float(&mut n, which, frac);
    }
    if rng.chance(0.08) {
        n.bypass = true;
    }
    n
}

/// A source node: a layer (sometimes late), a missing clip, a solid or a gradient.
fn random_source(rng: &mut Rng, id: u64) -> filmcraft_project::comp::CompNode {
    let kind = match rng.below(10) {
        0 => CompKind::MediaIn { source: MediaInSource::Item { item: filmcraft_project::ItemId(900_000 + rng.below(1000) as u64) }, offset: Tick::ZERO },
        1 => CompKind::Solid,
        2 => CompKind::Gradient,
        _ => {
            let offset = if rng.chance(0.2) { Tick((TICKS_PER_SECOND / 4).saturating_mul(1 + rng.below(8) as i64)) } else { Tick::ZERO };
            CompKind::MediaIn { source: MediaInSource::Layer { track: rng.below(4) as u32 }, offset }
        }
    };
    let mut n = new_node(id, kind, label(rng));
    if matches!(n.kind, CompKind::Solid | CompKind::Gradient) && rng.chance(0.5) {
        set_float(&mut n, 0, rng.f());
    }
    n
}

/// A random comp: one or two chains of processing nodes, merged when there are two; sometimes an
/// unconnected node or a Media Out without input.
fn random_comp(rng: &mut Rng) -> filmcraft_project::comp::Comp {
    let mut c = empty_comp();
    let mut next = 2u64;
    let mut fresh = || {
        let id = next;
        next += 1;
        id
    };
    if rng.chance(0.03) {
        return c;
    }
    let chains = 1 + usize::from(rng.chance(0.55));
    let mut ends = Vec::new();
    for _ in 0..chains {
        let src = fresh();
        c.nodes.push(random_source(rng, src));
        let mut end = src;
        for _ in 0..rng.below(3) {
            let id = fresh();
            c.nodes.push(random_node(rng, id));
            link(&mut c, end, id, 0);
            end = id;
        }
        ends.push(end);
    }
    let last = match ends.as_slice() {
        [bg, fg] => {
            let m = fresh();
            let mut n = new_node(m, CompKind::Merge, label(rng));
            if rng.chance(0.6) {
                set_float(&mut n, 0, rng.f());
            }
            c.nodes.push(n);
            link(&mut c, *bg, m, 0);
            link(&mut c, *fg, m, 1);
            m
        }
        [one, ..] => *one,
        [] => return c,
    };
    link(&mut c, last, 1, 0);
    if rng.chance(0.15) {
        let id = fresh();
        c.nodes.push(random_node(rng, id));
    }
    c
}

/// The facts' `kind` ("merge", "media_in", …).
fn kind_of(f: &CompFacts) -> String {
    serde_json::to_value(f).ok().and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string)).unwrap_or_default()
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    if let Some(p) = &args.dump {
        let lines: Vec<String> = comp_eval_cases()
            .iter()
            .map(|c| serde_json::json!({"label": c.label, "facts": c.facts, "template": c.template, "accept": c.facts.phrasings()}).to_string())
            .collect();
        std::fs::write(p, lines.join("\n") + "\n").map_err(|e| format!("{p}: {e}"))?;
        println!("wrote {} cases to {p}", lines.len());
        return Ok(());
    }
    std::fs::create_dir_all(&args.out_dir)?;
    let mut rng = Rng(args.seed ^ 0x636f_6d70_7472_6e64);
    let held: BTreeSet<String> =
        comp_eval_cases().iter().filter_map(|c| serde_json::to_string(&c.facts).ok()).map(|s| filmcraft_llm::compact_facts(&s)).collect();
    let open = |name: &str| std::fs::OpenOptions::new().create(true).append(true).open(args.out_dir.join(name));
    let mut train = std::io::BufWriter::new(open("train.jsonl")?);
    let mut val = std::io::BufWriter::new(open("val.jsonl")?);
    let project = Project::default();
    let (mut seen, mut kinds): (BTreeSet<String>, BTreeMap<String, usize>) = (BTreeSet::new(), BTreeMap::new());
    let (mut n_train, mut n_val, mut dropped_held, mut dropped_dup, mut rejected) = (0usize, 0usize, 0usize, 0usize, 0usize);
    for ci in 0..args.comps {
        let c = random_comp(&mut rng);
        let is_val = rng.chance(args.val);
        for (ni, facts) in comp_facts(&c, &project, Tick::ZERO).into_iter().enumerate() {
            let facts_json = serde_json::to_string(&facts)?;
            let compact = filmcraft_llm::compact_facts(&facts_json);
            if held.contains(&compact) {
                dropped_held += 1;
                continue;
            }
            if !seen.insert(compact) {
                dropped_dup += 1;
                continue;
            }
            let (system, user) = filmcraft_llm::explain_prompt(&facts_json, &facts.sentence());
            let mut targets = facts.phrasings();
            if rng.chance(0.5) {
                targets.reverse();
            }
            for target in targets.iter().take(args.per_node) {
                if check_comp_explanation(&facts, target).is_err() {
                    // too long for the checker (long labels, four parameters): the template is shown
                    rejected += 1;
                    continue;
                }
                let kind = kind_of(&facts);
                let line = serde_json::json!({
                    "id": format!("c{ci}.{ni}"),
                    "family": format!("comp_{kind}"),
                    "kind": "comp",
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": user},
                        {"role": "assistant", "content": target},
                    ],
                })
                .to_string();
                if is_val {
                    writeln!(val, "{line}")?;
                    n_val += 1;
                } else {
                    writeln!(train, "{line}")?;
                    n_train += 1;
                }
                *kinds.entry(kind).or_default() += 1;
            }
        }
    }
    train.flush()?;
    val.flush()?;
    let stats = serde_json::json!({
        "seed": args.seed,
        "comps": args.comps,
        "per_node": args.per_node,
        "train_examples": n_train,
        "val_examples": n_val,
        "dropped_held_out": dropped_held,
        "dropped_duplicate": dropped_dup,
        "rejected_phrasings": rejected,
        "kinds": kinds,
    });
    std::fs::write(args.out_dir.join("comp-data-stats.json"), serde_json::to_string_pretty(&stats)?)?;
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}
