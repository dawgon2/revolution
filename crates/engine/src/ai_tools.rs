//! AI Tools: see which AI tools hold a model in memory or have work running, and stop / free each
//! one or all of them (owner request 2026-10-24).
//!
//! | command | does |
//! |---|---|
//! | `ai.status` | one row per tool: its model loaded, jobs running |
//! | `ai.stop` | `{"tool":id?}`: stop that tool's work and free its models (every tool without `tool`) |
//!
//! Stopping is never destructive: a stopped job changes nothing (each job's own cancel rules), a
//! stopped assistant question gets a "stopped" answer, and every model loads again the next time
//! its tool is used. Voice Isolation's live model (DeepFilterNet3, 8 MB) is not freed: effects on
//! clips use it while you play, and without it they would quietly switch to DeNoise.

use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad};
use crate::{Result, Session};

/// The tools, in the order the AI Tools window lists them.
pub const TOOLS: &[(&str, &str)] = &[
    ("assistant", "Assistant and AI explanations"),
    ("vision", "Assistant picture model"),
    ("magicMask", "Magic Mask"),
    ("voice", "Voice clean-up and rendered Voice Isolation"),
    ("narration", "Text to Speech voices"),
];

/// One tool's state.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub id: &'static str,
    pub label: &'static str,
    /// A model is in memory.
    pub loaded: bool,
    /// What is loaded, when the tool can tell.
    pub model: Option<String>,
    /// Jobs or questions still running or waiting.
    pub running: usize,
}

/// Every tool's state (cheap; never waits on a running model).
pub fn status(s: &Session) -> Vec<ToolStatus> {
    TOOLS.iter().map(|&(id, label)| tool_status(s, id, label)).collect()
}

fn tool_status(s: &Session, id: &'static str, label: &'static str) -> ToolStatus {
    let (loaded, model, running) = match id {
        "assistant" => {
            let model = crate::ai_explain::real_backend().and_then(|b| b.loaded_name());
            let meaning = crate::coach_meaning::loaded();
            (model.is_some() || meaning, model, s.assist.pending())
        }
        "vision" => (crate::ai_explain::vision_loaded(), None, 0),
        "magicMask" => (crate::magic_mask::models_loaded(), None, s.magic_mask.live_jobs(None).len()),
        "voice" => (voice_loaded(), None, running_jobs(s, id).len()),
        "narration" => (narration_loaded(), None, running_jobs(s, id).len()),
        _ => (false, None, 0),
    };
    ToolStatus { id, label, loaded, model, running }
}

fn voice_loaded() -> bool {
    #[cfg(feature = "voice-isolation")]
    {
        filmcraft_voice::mossformer::status().1
    }
    #[cfg(not(feature = "voice-isolation"))]
    {
        false
    }
}

fn narration_loaded() -> bool {
    #[cfg(feature = "neural-voices")]
    {
        filmcraft_tts::neural::loaded()
    }
    #[cfg(not(feature = "neural-voices"))]
    {
        false
    }
}

/// The unfinished session jobs that belong to `tool`.
fn running_jobs(s: &Session, tool: &str) -> Vec<u64> {
    let labels: &[&str] = match tool {
        "voice" => &[crate::voice_cleanup::LABEL, crate::voice_isolation_render::LABEL],
        "narration" => &[crate::narration::SYNTH_LABEL],
        _ => &[],
    };
    s.jobs
        .iter()
        .filter(|j| !j.progress.finished.load(std::sync::atomic::Ordering::Relaxed))
        .filter(|j| labels.iter().any(|l| j.label.starts_with(l)))
        .map(|j| j.id)
        .collect()
}

/// Stop `tool`'s work and free its models. Returns (jobs / questions stopped, a model was freed).
fn stop_tool(s: &mut Session, tool: &str) -> Result<(usize, bool)> {
    let was_loaded = TOOLS.iter().find(|(id, _)| *id == tool).is_some_and(|&(id, label)| tool_status(s, id, label).loaded);
    let stopped = match tool {
        "assistant" => {
            let n = s.assist.stop_all();
            crate::ai_explain::abort_generations();
            s.ai.release();
            if let Some(b) = crate::ai_explain::real_backend() {
                b.release_now();
            }
            crate::coach_meaning::release();
            n
        }
        "vision" => {
            crate::ai_explain::release_vision();
            0
        }
        "magicMask" => {
            let n = s.magic_mask.live_jobs(None).len();
            if n > 0 {
                crate::magic_mask::cancel(s, &json!({}))?;
            }
            crate::magic_mask::release_models();
            n
        }
        "voice" | "narration" => {
            let jobs = running_jobs(s, tool);
            for j in s.jobs.iter().filter(|j| jobs.contains(&j.id)) {
                j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if tool == "voice" {
                #[cfg(feature = "voice-isolation")]
                filmcraft_voice::mossformer::release();
            } else {
                #[cfg(feature = "neural-voices")]
                filmcraft_tts::neural::release();
            }
            jobs.len()
        }
        other => return Err(bad("ai.stop", format!("unknown tool `{}` (one of: {})", other.chars().take(40).collect::<String>(), ids()))),
    };
    Ok((stopped, was_loaded))
}

fn ids() -> String {
    TOOLS.iter().map(|(id, _)| *id).collect::<Vec<_>>().join(", ")
}

fn status_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(serde_json::to_value(status(s)).unwrap_or_default())
}

fn stop_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let tools: Vec<&str> = match p.get("tool") {
        None | Some(Value::Null) => TOOLS.iter().map(|(id, _)| *id).collect(),
        Some(Value::String(t)) => vec![t.as_str()],
        Some(_) => return Err(bad("ai.stop", "`tool` must be a tool id")),
    };
    let mut rows = Vec::new();
    for t in tools {
        let (stopped, freed) = stop_tool(s, t)?;
        rows.push(json!({"tool": t, "stopped": stopped, "freed": freed}));
    }
    Ok(json!({"tools": rows, "status": status(s)}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec { id: "ai.status", label: "AI Tools Status", menu: &[], shortcut: None, params: "{}", enabled: always, run: status_cmd, journal: false },
        CommandSpec {
            id: "ai.stop",
            label: "Stop All AI and Free Memory",
            menu: &["Edit"],
            shortcut: None,
            params: r#"{"tool":"assistant|vision|magicMask|voice|narration"?}"#,
            enabled: always,
            run: stop_cmd,
            journal: false,
        },
    ]
}
