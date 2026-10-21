//! FORK-ONLY (AI-PLAN AI5 item 8, FORK-PLAN §1a.92): **Send to my training inbox**. Writes a whole
//! Assistant conversation (every turn's question, checked answer, guide sections, rating and
//! correction, page, and the ticked grade settings / picture facts / frame) as one JSON file to
//! `<data dir>/assistant-inbox/`, for the fork's runner to triage into held-out or training cases
//! (`tools/fork/ai-inbox-import.py`). Nothing goes over the network and nothing is stripped: it
//! never leaves this computer. Behind the engine feature `fork-inbox`; listed in
//! `docs/fork/FORK-ONLY.md`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;
use crate::coach_share::{Extras, Rating, ShareRecord};

/// The inbox folder's name inside the data directory.
pub const INBOX_DIR: &str = "assistant-inbox";
/// Most turns written from one conversation (the panel keeps 40).
pub const MAX_TURNS: usize = 40;
/// Largest inbox file (JSON text); bigger conversations are refused, not cut.
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
/// Most files kept in the inbox; past it new ones are refused until the runner imports some.
pub const MAX_FILES: usize = 2_000;

/// `<data dir>/assistant-inbox/` (follows `--data-dir`); `None` without a data directory.
pub fn inbox_dir() -> Option<PathBuf> {
    crate::autosave::data_dir().map(|d| d.join(INBOX_DIR))
}

/// One turn as JSON.
fn turn_json(id: u64, r: &ShareRecord, answer: Option<Value>, extras: Extras) -> Value {
    json!({
        "id": id,
        "question": r.question,
        "page": r.page,
        "answer": answer,
        "rating": match r.rating { Some(Rating::Up) => Some("up"), Some(Rating::Down) => Some("down"), None => None },
        "correction": r.correction,
        "gradeFacts": if extras.grade { r.grade_facts.clone() } else { None },
        "pictureFacts": if extras.picture { r.picture_facts.clone() } else { None },
    })
}

/// The conversation file's JSON. `turns` are (id, record, the answer as `coach.assistResult`
/// gives it), oldest first.
pub fn conversation_json(turns: &[(u64, ShareRecord, Option<Value>)], extras: Extras, frame_file: Option<&str>, sent_unix: u64) -> Value {
    let list: Vec<Value> = turns.iter().take(MAX_TURNS).map(|(id, r, a)| turn_json(*id, r, a.clone(), extras)).collect();
    json!({
        "format": "filmcraft-assistant-inbox/1",
        "origin": "owner-inbox",
        "filmcraft": env!("CARGO_PKG_VERSION"),
        "sent": sent_unix,
        "conversation": turns.first().map(|(id, _, _)| *id),
        "turns": list,
        "frame": frame_file,
    })
}

/// Writes the conversation (and the frame, PNG) to `dir`; the JSON file's path.
pub fn write(dir: &Path, conversation: &Value, frame_png: Option<&[u8]>, stem: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make the inbox folder: {e}"))?;
    let files = std::fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "json")).count()).unwrap_or(0);
    if files >= MAX_FILES {
        return Err(format!("the training inbox holds {MAX_FILES} conversations already; import them first"));
    }
    let text = serde_json::to_string_pretty(conversation).map_err(|e| e.to_string())?;
    if text.len() > MAX_FILE_BYTES {
        return Err("this conversation is too large for the training inbox".into());
    }
    if let Some(png) = frame_png {
        filmcraft_format::atomic_write(&dir.join(format!("{stem}.png")), png).map_err(|e| format!("couldn't save the frame: {e}"))?;
    }
    let path = dir.join(format!("{stem}.json"));
    filmcraft_format::atomic_write(&path, text.as_bytes()).map_err(|e| format!("couldn't save to the training inbox: {e}"))?;
    Ok(path)
}

/// `coach.shareExample` `route: inbox`: the conversation of `ids` (oldest first; `id` alone when
/// none are given) to [`inbox_dir`].
pub fn send(s: &mut Session, p: &Value, id: u64, extras: Extras, frame_png: Option<Vec<u8>>) -> Result<Value, String> {
    let ids: Vec<u64> = match p.get("conversation").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).take(MAX_TURNS).collect(),
        None => vec![id],
    };
    let turns: Vec<(u64, ShareRecord, Option<Value>)> =
        ids.iter().filter_map(|i| s.assist.records.get(*i).map(|r| (*i, r, s.assist.result(*i).flatten().map(|a| a.shareable_json())))).collect();
    if turns.is_empty() {
        return Err("none of those questions is kept any more".into());
    }
    let since = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default();
    let sent = since.as_secs();
    // milliseconds in the name: two sends in the same second don't overwrite each other
    let millis = u64::try_from(since.as_millis()).unwrap_or(u64::MAX);
    let first = turns.first().map_or(id, |(i, _, _)| *i);
    let stem = format!("conversation-{millis}-{first}");
    let frame_name = frame_png.as_ref().map(|_| format!("{stem}.png"));
    let conv = conversation_json(&turns, extras, frame_name.as_deref(), sent);
    let dir = inbox_dir().ok_or("this computer has no Revolution data folder to keep the inbox in")?;
    let path = write(&dir, &conv, frame_png.as_deref(), &stem)?;
    Ok(json!({"path": path.display().to_string(), "turns": turns.len(), "frame": frame_name.is_some()}))
}

#[cfg(test)]
#[path = "fork_inbox_tests.rs"]
mod tests;
