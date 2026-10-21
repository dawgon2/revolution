//! The comp-graph explainer (fork COMPOSITOR-PLAN C1 item 7): what a comp does, node by node, in
//! plain words, in the order the pictures flow. Template sentences built only from the graph
//! itself (node names, real parameter labels and values that differ from their defaults), so they
//! can't name a control or a node that isn't there. `comp.explain` returns them; a local model
//! may later reword them under the same check as the grade explainer (G2.11).
//!
//! Fork AI2: each line is built from [`CompFacts`] (serde, the explainer model's prompt facts);
//! [`check_comp_explanation`] accepts a model's line only when it is one of the line's fixed
//! phrasings ([`CompFacts::phrasings`]) with the real names and values, as the branch notes do.

use filmcraft_project::comp::{self, Comp, CompKind, CompNode, MediaInSource};
use filmcraft_project::dag::NodeId;
use filmcraft_project::effect::{ParamKind, find_effect};
use filmcraft_project::{ParamValue, Project};
use filmcraft_render::grade_explain::{FactMismatch, check_phrasings};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};

/// Longest node label quoted in a sentence, in characters.
const MAX_LABEL: usize = 60;
/// Most changed parameters listed per node.
const MAX_PARAMS_LISTED: usize = 4;

/// One node's sentence.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub node: NodeId,
    /// The node's name as the canvas shows it (its label, else kind and id).
    pub name: String,
    pub text: String,
}

/// The explanation: a one-line summary and the nodes in evaluation order, then the nodes that
/// don't reach Media Out.
#[derive(Clone, Debug, PartialEq)]
pub struct Explanation {
    pub summary: String,
    pub lines: Vec<Line>,
    pub unused: Vec<Line>,
}

/// The on-screen name of a node kind (as the Compositor's canvas and Add Node menu show it).
pub fn kind_label(key: &str) -> &'static str {
    match key {
        "mediaIn" => "Media In",
        "mediaOut" => "Media Out",
        "merge" => "Merge",
        "transform" => "Transform",
        "key" => "Key",
        "mask" => "Mask",
        "blur" => "Blur",
        "color" => "Color",
        "text" => "Text",
        "solid" => "Solid",
        "gradient" => "Gradient",
        _ => "Node",
    }
}

/// A node's name: its label (cut to [`MAX_LABEL`] characters), else "Kind id".
pub fn node_name(n: &CompNode) -> String {
    let label: String = n.label.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect();
    if label.trim().is_empty() { format!("{} {}", kind_label(n.kind.key()), n.id) } else { label.trim().to_string() }
}

/// Explains `c` at comp time `t`.
pub fn explain(c: &Comp, project: &Project, t: Tick) -> Explanation {
    let name_of = |id: NodeId| c.node(id).map_or_else(|| format!("node {id}"), node_name);
    let input = |id: NodeId, port: u8| c.edges.iter().find(|e| e.to.node == id && e.to.index == port).map(|e| name_of(e.from.node));
    let order = match c.validate() {
        Ok(o) => o.nodes().collect::<Vec<_>>(),
        Err(e) => {
            return Explanation {
                summary: format!("This comp can't be shown: {e}. It renders transparent until that is fixed."),
                lines: Vec::new(),
                unused: Vec::new(),
            };
        }
    };
    let line = |n: &CompNode| Line { node: n.id, name: node_name(n), text: node_facts(n, project, t, &input).text(0) };
    let lines: Vec<Line> = order.iter().filter_map(|id| c.node(*id)).map(line).collect();
    let unused: Vec<Line> = c
        .nodes
        .iter()
        .filter(|n| !order.contains(&n.id))
        .map(|n| Line { node: n.id, name: node_name(n), text: CompFacts::Unused { node: node_name(n) }.text(0) })
        .collect();
    let summary = match c.output.and_then(|o| c.node(o)) {
        None => "This comp has no Media Out node, so it is transparent.".to_string(),
        Some(out) => match input(out.id, 0) {
            None => format!("{} has no input, so the comp is transparent.", node_name(out)),
            Some(_) => {
                let sources = lines.iter().filter(|l| c.node(l.node).is_some_and(|n| n.kind.inputs() == 0)).count();
                format!("{} node(s) build the picture from {} source(s); the result is what {} shows.", lines.len(), sources, node_name(out))
            }
        },
    };
    Explanation { summary, lines, unused }
}

/// What one comp node does, read from the graph (the facts behind one line). `node` is the
/// node's name ([`node_name`]); inputs are named the same way; `changed` holds "Label value" for
/// up to [`MAX_PARAMS_LISTED`] parameters off their defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompFacts {
    /// `later`: how much later than the comp the source runs ("0.50 s"), when it isn't in step.
    MediaIn {
        node: String,
        from: String,
        later: Option<String>,
    },
    MediaOut {
        node: String,
        shows: Option<String>,
    },
    Merge {
        node: String,
        foreground: String,
        background: String,
        changed: Vec<String>,
    },
    Transform {
        node: String,
        input: String,
        changed: Vec<String>,
    },
    Key {
        node: String,
        input: String,
        key: String,
        changed: Vec<String>,
    },
    Mask {
        node: String,
        input: String,
        masks: usize,
    },
    Blur {
        node: String,
        input: String,
        changed: Vec<String>,
    },
    Color {
        node: String,
        input: String,
    },
    Text {
        node: String,
    },
    Solid {
        node: String,
        changed: Vec<String>,
    },
    Gradient {
        node: String,
        changed: Vec<String>,
    },
    /// A switched-off node with an input.
    Bypassed {
        node: String,
        input: String,
    },
    /// A node that doesn't reach Media Out.
    Unused {
        node: String,
    },
}

/// The phrasings of each kind of line: 0 is the template line `comp.explain` shows.
const VARIANTS: usize = 2;

/// The facts of every node of `c` at `t`: the nodes in flow order, then the unused ones. Empty
/// for a comp that doesn't validate (its summary says why).
pub fn comp_facts(c: &Comp, project: &Project, t: Tick) -> Vec<CompFacts> {
    let name_of = |id: NodeId| c.node(id).map_or_else(|| format!("node {id}"), node_name);
    let input = |id: NodeId, port: u8| c.edges.iter().find(|e| e.to.node == id && e.to.index == port).map(|e| name_of(e.from.node));
    let Ok(order) = c.validate().map(|o| o.nodes().collect::<Vec<_>>()) else { return Vec::new() };
    let mut out: Vec<CompFacts> = order.iter().filter_map(|id| c.node(*id)).map(|n| node_facts(n, project, t, &input)).collect();
    out.extend(c.nodes.iter().filter(|n| !order.contains(&n.id)).map(|n| CompFacts::Unused { node: node_name(n) }));
    out
}

fn node_facts(n: &CompNode, project: &Project, t: Tick, input: &dyn Fn(NodeId, u8) -> Option<String>) -> CompFacts {
    let node = node_name(n);
    let a = || input(n.id, 0).unwrap_or_else(|| "nothing (no input connected)".to_string());
    if n.bypass && n.kind.inputs() > 0 {
        return CompFacts::Bypassed { node, input: a() };
    }
    let changed = changed_params(n, t);
    match &n.kind {
        CompKind::MediaIn { source, offset } => {
            let from = match source {
                MediaInSource::Item { item } => match project.item(*item) {
                    Some(i) => format!("the clip \u{201c}{}\u{201d}", i.name.chars().filter(|c| !c.is_control()).take(MAX_LABEL).collect::<String>()),
                    None => "a clip that is no longer in the project (it shows nothing)".to_string(),
                },
                MediaInSource::Layer { track } => format!("track V{} under the comp clip", track.saturating_add(1)),
            };
            let later = (offset.0 != 0).then(|| format!("{:.2} s", offset.0 as f64 / TICKS_PER_SECOND as f64));
            CompFacts::MediaIn { node, from, later }
        }
        CompKind::MediaOut => CompFacts::MediaOut { node, shows: input(n.id, 0) },
        CompKind::Merge => CompFacts::Merge {
            node,
            foreground: input(n.id, 1).unwrap_or_else(|| "nothing".to_string()),
            background: input(n.id, 0).unwrap_or_else(|| "transparency".to_string()),
            changed,
        },
        CompKind::Transform => CompFacts::Transform { node, input: a(), changed },
        CompKind::Key { effect } => CompFacts::Key { node, input: a(), key: find_effect(effect).map_or("a key", |d| d.name).to_string(), changed },
        CompKind::Mask { masks } => CompFacts::Mask { node, input: a(), masks: masks.len() },
        CompKind::Blur => CompFacts::Blur { node, input: a(), changed },
        CompKind::Color { .. } => CompFacts::Color { node, input: a() },
        CompKind::Text { .. } => CompFacts::Text { node },
        CompKind::Solid => CompFacts::Solid { node, changed },
        CompKind::Gradient => CompFacts::Gradient { node, changed },
    }
}

impl CompFacts {
    /// The node the line is about.
    pub fn node(&self) -> &str {
        match self {
            Self::MediaIn { node, .. }
            | Self::MediaOut { node, .. }
            | Self::Merge { node, .. }
            | Self::Transform { node, .. }
            | Self::Key { node, .. }
            | Self::Mask { node, .. }
            | Self::Blur { node, .. }
            | Self::Color { node, .. }
            | Self::Text { node }
            | Self::Solid { node, .. }
            | Self::Gradient { node, .. }
            | Self::Bypassed { node, .. }
            | Self::Unused { node } => node,
        }
    }

    /// The template line with the node's name in front: "Blur 3 blurs …" (what the model writes).
    pub fn sentence(&self) -> String {
        format!("{} {}", self.node(), self.text(0))
    }

    /// Every accepted phrasing of the line (with the node's name in front), the template first.
    pub fn phrasings(&self) -> Vec<String> {
        let mut v: Vec<String> = (0..VARIANTS).map(|k| format!("{} {}", self.node(), self.text(k))).collect();
        v.dedup();
        v
    }

    /// Phrasing `k` without the node's name (0: the text `comp.explain` shows after the name).
    pub fn text(&self, k: usize) -> String {
        let with = |changed: &[String]| if changed.is_empty() { String::new() } else { format!(" ({})", changed.join(", ")) };
        match self {
            Self::MediaIn { from, later, .. } => {
                let later = later.as_ref().map_or(String::new(), |l| format!(", {l} later than the comp"));
                if k == 0 { format!("reads {from}{later}.") } else { format!("takes its picture from {from}{later}.") }
            }
            Self::MediaOut { shows: Some(i), .. } => {
                if k == 0 {
                    format!("is the comp's result: it shows {i}.")
                } else {
                    format!("shows {i} as the comp's result.")
                }
            }
            Self::MediaOut { shows: None, .. } => "has no input, so the comp is transparent.".to_string(),
            Self::Merge { foreground, background, changed, .. } => {
                let w = with(changed);
                if k == 0 {
                    format!("puts {foreground} (foreground) over {background} (background){w}.")
                } else {
                    format!("lays {foreground} on top of {background}{w}.")
                }
            }
            Self::Transform { input, changed, .. } if changed.is_empty() => {
                if k == 0 {
                    format!("passes {input} through unchanged (every setting at its default).")
                } else {
                    format!("leaves {input} as it is (every setting at its default).")
                }
            }
            Self::Transform { input, changed, .. } => {
                let w = with(changed);
                if k == 0 { format!("moves, scales or turns {input}{w}.") } else { format!("changes the position, size or angle of {input}{w}.") }
            }
            Self::Key { input, key, changed, .. } => {
                let w = with(changed);
                if k == 0 { format!("makes part of {input} transparent with {key}{w}.") } else { format!("keys out part of {input} with {key}{w}.") }
            }
            Self::Mask { input, masks, .. } => {
                if k == 0 {
                    format!("keeps only the parts of {input} inside its {masks} mask(s).")
                } else {
                    format!("hides everything of {input} outside its {masks} mask(s).")
                }
            }
            Self::Blur { input, changed, .. } => {
                let w = with(changed);
                if k == 0 { format!("blurs {input}{w}.") } else { format!("softens {input}{w}.") }
            }
            Self::Color { input, .. } => {
                if k == 0 {
                    format!("grades {input} with one colour node (open it to see its corrections).")
                } else {
                    format!("changes the colours of {input} with one colour node (open it to see its corrections).")
                }
            }
            Self::Text { .. } => if k == 0 { "draws a text layer." } else { "adds a text layer." }.to_string(),
            Self::Solid { changed, .. } => {
                let w = with(changed);
                if k == 0 { format!("is a flat colour{w}.") } else { format!("is one plain colour{w}.") }
            }
            Self::Gradient { changed, .. } => {
                let w = with(changed);
                if k == 0 { format!("is a gradient between two colours{w}.") } else { format!("blends two colours in a gradient{w}.") }
            }
            Self::Bypassed { input, .. } => {
                if k == 0 {
                    format!("is bypassed: {input} passes through unchanged.")
                } else {
                    format!("is switched off, so {input} passes through unchanged.")
                }
            }
            Self::Unused { .. } => {
                if k == 0 {
                    "isn't connected to Media Out, so it changes nothing.".to_string()
                } else {
                    "doesn't reach Media Out, so it has no effect.".to_string()
                }
            }
        }
    }
}

/// Check that `text` truthfully explains one comp node: one of [`CompFacts::phrasings`] (case,
/// spacing, "color" / "colour" and a final full stop aside). Err says why not; then show
/// [`CompFacts::sentence`]. The text is hostile (any UTF-8, any length).
pub fn check_comp_explanation(facts: &CompFacts, text: &str) -> Result<(), FactMismatch> {
    check_phrasings(&facts.phrasings(), text)
}

/// "Label value" for the parameters that differ from their defaults at `t` (at most
/// [`MAX_PARAMS_LISTED`]; colours aren't listed).
fn changed_params(n: &CompNode, t: Tick) -> Vec<String> {
    let mut out = Vec::new();
    for d in comp::params(&n.kind) {
        if out.len() >= MAX_PARAMS_LISTED {
            break;
        }
        let Some(v) = n.value_at(d.id, t) else { continue };
        if v == d.default {
            continue;
        }
        let text = match (&v, &d.kind) {
            (ParamValue::Float(f), ParamKind::Float { unit, decimals, .. }) if f.is_finite() => {
                format!("{} {:.*}{}", d.label, usize::from((*decimals).min(3)), f, unit)
            }
            (ParamValue::Float(f), ParamKind::Angle) if f.is_finite() => format!("{} {f:.1}°", d.label),
            (ParamValue::Choice(i), ParamKind::Choice(names)) => match names.get(*i as usize) {
                Some(name) => format!("{} {name}", d.label),
                None => continue,
            },
            (ParamValue::Bool(b), _) => format!("{} {}", d.label, if *b { "on" } else { "off" }),
            (ParamValue::Vec2(p), _) if p.x.is_finite() && p.y.is_finite() => format!("{} {:.0}, {:.0}", d.label, p.x, p.y),
            _ => continue,
        };
        out.push(text);
    }
    out
}

// ---- held-out set 4 and the training data's comp builders (fork AI2) ----------------------------

/// One case of the comp held-out set (fork AI2 set 4): the facts of one node of a fixed comp and
/// its template line. Never in the training data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompEvalCase {
    /// A short id, e.g. `c4_scaled_layer_1` (the comp's name and the line's position).
    pub label: String,
    pub facts: CompFacts,
    /// The template line ([`CompFacts::sentence`]).
    pub template: String,
}

/// The float parameters of a node kind: (key, slider range, decimals).
#[doc(hidden)]
pub fn float_params(kind: &CompKind) -> Vec<(&'static str, f64, f64, u8)> {
    comp::params(kind)
        .into_iter()
        .filter_map(|d| match d.kind {
            ParamKind::Float { soft_min, soft_max, decimals, .. } if soft_min.is_finite() && soft_max.is_finite() && soft_max > soft_min => {
                Some((d.id, soft_min, soft_max, decimals))
            }
            _ => None,
        })
        .collect()
}

/// Sets float parameter `which` of `n` (in [`float_params`] order) to `frac` of its slider range,
/// rounded to the parameter's decimals. False when the kind has no such parameter.
#[doc(hidden)]
pub fn set_float(n: &mut CompNode, which: usize, frac: f64) -> bool {
    let Some((key, lo, hi, decimals)) = float_params(&n.kind).get(which).copied() else { return false };
    let scale = 10f64.powi(i32::from(decimals.min(3)));
    let v = ((lo + (hi - lo) * frac.clamp(0.0, 1.0)) * scale).round() / scale;
    n.params.insert(key.to_string(), filmcraft_project::Param::new(ParamValue::Float(v)));
    true
}

/// A node `id` of `kind`, labelled `label` when not empty.
#[doc(hidden)]
pub fn new_node(id: NodeId, kind: CompKind, label: &str) -> CompNode {
    let mut n = CompNode::new(id, kind);
    n.label = label.to_string();
    n
}

/// `n` with float parameter `which` set by [`set_float`].
#[doc(hidden)]
pub fn with_float(mut n: CompNode, which: usize, frac: f64) -> CompNode {
    set_float(&mut n, which, frac);
    n
}

/// Connects `from`'s picture to input `port` of `to`.
#[doc(hidden)]
pub fn link(c: &mut Comp, from: NodeId, to: NodeId, port: u8) {
    use filmcraft_project::dag::{Edge, Port};
    c.edges.push(Edge::new(Port::new(from, 0), Port::new(to, port)));
}

/// A fresh 1080p comp holding only Media Out (node 1).
#[doc(hidden)]
pub fn empty_comp() -> Comp {
    Comp::new(1920, 1080, filmcraft_time::FrameRate::FPS_24, Tick(TICKS_PER_SECOND.saturating_mul(10)))
}

/// Fixed comps covering every node kind but Text: a scaled layer, a Merge at half opacity, a keyed
/// foreground over a solid, a blur with an unused gradient, a mask without masks, a bypassed
/// Transform, a source missing from the project, a late source through a Color node, a gradient
/// under a layer, Media Out with no input.
pub fn comp_eval_cases() -> Vec<CompEvalCase> {
    let layer = |track: u32| CompKind::MediaIn { source: MediaInSource::Layer { track }, offset: Tick::ZERO };
    let mut comps: Vec<(&str, Comp)> = Vec::new();
    let mut c = empty_comp();
    c.nodes.push(new_node(2, layer(0), ""));
    c.nodes.push(with_float(new_node(3, CompKind::Transform, "Push in"), 0, 0.75));
    link(&mut c, 2, 3, 0);
    link(&mut c, 3, 1, 0);
    comps.push(("scaled_layer", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, layer(0), "Plate"));
    c.nodes.push(new_node(3, layer(1), "Actor"));
    c.nodes.push(with_float(new_node(4, CompKind::Merge, ""), 0, 0.5));
    link(&mut c, 2, 4, 0);
    link(&mut c, 3, 4, 1);
    link(&mut c, 4, 1, 0);
    comps.push(("merge_half", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, CompKind::Solid, "Backdrop"));
    c.nodes.push(new_node(3, layer(1), ""));
    c.nodes.push(new_node(4, CompKind::Key { effect: "ultra_key".into() }, "Green screen"));
    c.nodes.push(new_node(5, CompKind::Merge, ""));
    link(&mut c, 3, 4, 0);
    link(&mut c, 2, 5, 0);
    link(&mut c, 4, 5, 1);
    link(&mut c, 5, 1, 0);
    comps.push(("keyed_over_solid", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, layer(0), ""));
    c.nodes.push(with_float(new_node(3, CompKind::Blur, "Soften"), 0, 0.2));
    c.nodes.push(new_node(4, CompKind::Gradient, ""));
    link(&mut c, 2, 3, 0);
    link(&mut c, 3, 1, 0);
    comps.push(("blur_and_unused", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, layer(2), ""));
    c.nodes.push(new_node(3, CompKind::Mask { masks: Vec::new() }, "Vignette"));
    link(&mut c, 2, 3, 0);
    link(&mut c, 3, 1, 0);
    comps.push(("mask_empty", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, layer(0), ""));
    let mut t = with_float(new_node(3, CompKind::Transform, ""), 0, 0.3);
    t.bypass = true;
    c.nodes.push(t);
    link(&mut c, 2, 3, 0);
    link(&mut c, 3, 1, 0);
    comps.push(("bypassed_transform", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, CompKind::MediaIn { source: MediaInSource::Item { item: filmcraft_project::ItemId(987_654) }, offset: Tick::ZERO }, ""));
    link(&mut c, 2, 1, 0);
    comps.push(("missing_source", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, CompKind::MediaIn { source: MediaInSource::Layer { track: 0 }, offset: Tick(TICKS_PER_SECOND / 2) }, ""));
    c.nodes.push(new_node(3, CompKind::Color { node: filmcraft_project::grade::GradeNode::new(1) }, "Warm"));
    link(&mut c, 2, 3, 0);
    link(&mut c, 3, 1, 0);
    comps.push(("late_through_color", c));
    let mut c = empty_comp();
    c.nodes.push(new_node(2, CompKind::Gradient, "Sky fill"));
    c.nodes.push(new_node(3, layer(0), ""));
    c.nodes.push(with_float(new_node(4, CompKind::Merge, "Comp"), 0, 0.8));
    link(&mut c, 2, 4, 0);
    link(&mut c, 3, 4, 1);
    link(&mut c, 4, 1, 0);
    comps.push(("gradient_under_layer", c));
    comps.push(("no_output_input", empty_comp()));
    let project = Project::default();
    let mut out = Vec::new();
    for (name, c) in comps {
        for (i, facts) in comp_facts(&c, &project, Tick::ZERO).into_iter().enumerate() {
            out.push(CompEvalCase { label: format!("c4_{name}_{i}"), template: facts.sentence(), facts });
        }
    }
    out
}

#[cfg(test)]
mod facts_tests {
    use super::*;

    #[test]
    fn lines_are_built_from_the_facts_and_every_phrasing_passes() {
        let cases = comp_eval_cases();
        assert!(cases.len() >= 25, "{}", cases.len());
        let mut labels: Vec<&str> = cases.iter().map(|c| c.label.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), cases.len());
        for kind in ["media_in", "media_out", "merge", "transform", "key", "mask", "blur", "color", "solid", "gradient", "bypassed", "unused"] {
            assert!(cases.iter().any(|c| serde_json::to_string(&c.facts).unwrap_or_default().contains(&format!("\"kind\":\"{kind}\""))), "{kind}");
        }
        for c in &cases {
            let p = c.facts.phrasings();
            assert_eq!(p.first(), Some(&c.template));
            for s in &p {
                assert_eq!(check_comp_explanation(&c.facts, s), Ok(()), "{}: {s}", c.label);
            }
            let json = serde_json::to_string(&c.facts).unwrap_or_default();
            assert_eq!(serde_json::from_str::<CompFacts>(&json).ok().as_ref(), Some(&c.facts));
        }
        // the lines comp.explain shows are the facts' template text
        let mut c = empty_comp();
        c.nodes.push(new_node(2, CompKind::Blur, "Soften"));
        link(&mut c, 2, 1, 0);
        let e = explain(&c, &Project::default(), Tick::ZERO);
        let f = comp_facts(&c, &Project::default(), Tick::ZERO);
        assert_eq!(e.lines.iter().map(|l| format!("{} {}", l.name, l.text)).collect::<Vec<_>>(), f.iter().map(CompFacts::sentence).collect::<Vec<_>>());
    }

    /// AI2 acceptance: no crash on hostile graphs. Every facts list built here must also check
    /// against its own template line.
    #[test]
    fn hostile_comps_do_not_panic() {
        let project = Project::default();
        let mut comps = Vec::new();
        // a cycle: no facts (the comp doesn't validate)
        let mut c = empty_comp();
        c.nodes.push(new_node(2, CompKind::Blur, ""));
        c.nodes.push(new_node(3, CompKind::Transform, ""));
        link(&mut c, 2, 3, 0);
        link(&mut c, 3, 2, 0);
        link(&mut c, 3, 1, 0);
        assert!(comp_facts(&c, &project, Tick::ZERO).is_empty());
        comps.push(c);
        // edges from / to nodes that don't exist, absurd ports, duplicate edges into one port
        let mut c = empty_comp();
        c.nodes.push(new_node(2, CompKind::Merge, ""));
        link(&mut c, 99, 2, 0);
        link(&mut c, 2, 77, 3);
        link(&mut c, 2, 1, u8::MAX);
        link(&mut c, 2, 1, 0);
        link(&mut c, 2, 1, 0);
        comps.push(c);
        // hostile values: NaN / infinite floats, out-of-range choices, extreme offsets and tracks,
        // control characters and a huge label, a source missing from the project, a node id at the top
        let mut c = empty_comp();
        let label = format!("\u{202e}{}\n\0", "x".repeat(100_000));
        let mut prev = None;
        let kinds = [
            CompKind::MediaIn { source: MediaInSource::Layer { track: u32::MAX }, offset: Tick(i64::MIN) },
            CompKind::MediaIn { source: MediaInSource::Item { item: filmcraft_project::ItemId(u64::MAX) }, offset: Tick(i64::MAX) },
            CompKind::Merge,
            CompKind::Transform,
            CompKind::Key { effect: "no such key \u{0007}".to_string() },
            CompKind::Mask { masks: Vec::new() },
            CompKind::Blur,
            CompKind::Solid,
            CompKind::Gradient,
        ];
        for (i, kind) in kinds.into_iter().enumerate() {
            let id = NodeId::MAX - 20 + i as NodeId;
            let mut n = new_node(id, kind, if i % 2 == 0 { &label } else { "" });
            for d in comp::params(&n.kind) {
                let v = match d.kind {
                    ParamKind::Choice(_) => ParamValue::Choice(u32::MAX),
                    _ if i % 2 == 0 => ParamValue::Float(f64::NAN),
                    _ => ParamValue::Float(f64::INFINITY),
                };
                n.params.insert(d.id.to_string(), filmcraft_project::Param::new(v));
            }
            n.bypass = i == 3;
            c.nodes.push(n);
            if let Some(p) = prev {
                link(&mut c, p, id, 0);
            }
            prev = Some(id);
        }
        if let Some(p) = prev {
            link(&mut c, p, 1, 0);
        }
        comps.push(c);
        // a long chain (deep graph)
        let mut c = empty_comp();
        let mut prev = None;
        for id in 2..2_000 {
            c.nodes.push(new_node(id, CompKind::Blur, ""));
            if let Some(p) = prev {
                link(&mut c, p, id, 0);
            }
            prev = Some(id);
        }
        if let Some(p) = prev {
            link(&mut c, p, 1, 0);
        }
        comps.push(c);
        for c in &comps {
            for t in [Tick::ZERO, Tick(i64::MIN), Tick(i64::MAX)] {
                let facts = comp_facts(c, &project, t);
                let _ = explain(c, &project, t);
                for f in &facts {
                    let s = f.sentence();
                    assert!(s.chars().count() < 2_000, "{s:.80}");
                    assert_eq!(check_comp_explanation(f, &s), Ok(()), "{s:.200}");
                    let _ = serde_json::to_string(f);
                }
            }
        }
    }

    #[test]
    fn invented_values_names_and_claims_are_rejected() {
        let cases = comp_eval_cases();
        let merge = cases.iter().find(|c| matches!(c.facts, CompFacts::Merge { .. }) && c.label.starts_with("c4_merge_half")).map(|c| c.facts.clone());
        let Some(merge) = merge else { panic!("no merge case") };
        let s = merge.sentence();
        assert!(s.contains("Actor (foreground) over Plate (background)"), "{s}");
        assert!(check_comp_explanation(&merge, &s.replace("Actor (foreground) over Plate", "Plate (foreground) over Actor")).is_err());
        assert!(check_comp_explanation(&merge, &s.replace("Plate", "Sky")).is_err());
        assert!(check_comp_explanation(&merge, &format!("{} It also blurs Actor.", s.trim_end_matches('.'))).is_err());
        for c in &cases {
            for t in ["", "NaN", "1e999", &"x".repeat(100_000), "\u{202e}"] {
                assert!(check_comp_explanation(&c.facts, t).is_err(), "{}: {t:.20}", c.label);
            }
        }
    }
}
