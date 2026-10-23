//! Saved comp templates (`comp.template.*`, fork C4.4, ADR 0009 §3): save selected comp nodes as
//! `<data dir>/templates/comp/<name>.fctemplate`, list them, insert one into a comp (one undo
//! step) and delete one. The model is [`filmcraft_project::comp_template`].
//!
//! Template files are hostile input: only regular files (no symbolic links) are read, at most
//! 4 MiB whatever their metadata says, and they go through [`CompTemplate::parse`]. Names become
//! file names only after [`safe_name`] refuses anything that could leave the folder (separators,
//! `..`, control characters, reserved Windows device names). Saves are atomic (a unique temporary
//! file, synced, then renamed); a temporary file a crash left behind is swept by the next list
//! once it is an hour old.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use filmcraft_project::comp_template::{CompTemplate, EXTENSION, MAX_BYTES, MAX_NAME};
use filmcraft_project::dag::NodeId;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::comp::{comp_at, edit_comp, target};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;

/// Most template files `comp.template.list` lists.
pub const MAX_LISTED: usize = 2_000;
/// Most folder entries one look at the folder goes through (templates, other files, temporaries).
const MAX_SCANNED: usize = 100_000;
/// Most template bytes one `comp.template.list` parses; later files are listed without counts.
pub const LIST_BUDGET: u64 = 64 * 1024 * 1024;
/// A save's temporary file older than this is swept by `comp.template.list`.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);
/// Makes each save's temporary file name unique within the process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        // save and delete change files, not the project: not journalled
        spec("comp.template.save", "Save Nodes as Template", r#"{"comp":id?,"group":[id]?,"nodes":[id],"name":str,"replace":bool=false}"#, save, false),
        spec("comp.template.list", "List Comp Templates", r#"{"namesOnly":bool=false}"#, list, false),
        // replaying it would depend on a file outside the project: not journalled
        spec("comp.template.insert", "Insert Comp Template", r#"{"comp":id?,"group":[id]?,"name":str,"at":[x,y]?}"#, insert, false),
        spec("comp.template.delete", "Delete Comp Template", r#"{"name":str}"#, delete, false),
    ]
}

/// Where comp templates live: `<data dir>/templates/comp` (None without a data directory).
pub fn templates_dir(s: &Session) -> Option<PathBuf> {
    s.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("templates").join("comp"))
}

/// Names Windows reserves for devices (with or without an extension), besides `COM` / `LPT`
/// plus a digit (see [`is_reserved`]).
const RESERVED: &[&str] = &["con", "prn", "aux", "nul", "conin$", "conout$"];

/// Is `stem` (lower case) a Windows device name: [`RESERVED`], or `com` / `lpt` followed by one
/// digit 0-9 or a superscript ¹ ² ³?
fn is_reserved(stem: &str) -> bool {
    if RESERVED.contains(&stem) {
        return true;
    }
    let Some(rest) = stem.strip_prefix("com").or_else(|| stem.strip_prefix("lpt")) else { return false };
    let mut chars = rest.chars();
    matches!((chars.next(), chars.next()), (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None))
}

/// A template name that is safe as a file name, or a plain-language reason it isn't: not empty,
/// at most [`MAX_NAME`] characters, no path separators, `..`, control characters or characters
/// Windows forbids, not starting or ending with a dot, not a reserved device name (any case, with
/// or without an extension). Surrounding whitespace is trimmed first.
pub fn safe_name(name: &str) -> std::result::Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("the template needs a name".into());
    }
    if n.chars().count() > MAX_NAME {
        return Err(format!("template names can be at most {MAX_NAME} characters"));
    }
    if n.chars().any(|c| c.is_control()) {
        return Err("template names can't contain control characters".into());
    }
    if let Some(c) = n.chars().find(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(format!("template names can't contain \u{201c}{c}\u{201d}"));
    }
    if n.contains("..") || n.starts_with('.') || n.ends_with('.') {
        return Err("template names can't start or end with a dot or contain \u{201c}..\u{201d}".into());
    }
    let stem = n.split('.').next().unwrap_or(n).trim_end().to_lowercase();
    if is_reserved(&stem) {
        return Err(format!("\u{201c}{n}\u{201d} is a name some systems reserve for devices; pick another name"));
    }
    Ok(n.to_string())
}

fn path_of(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.{EXTENSION}"))
}

fn dir_or_err(s: &Session, cmd: &str) -> Result<PathBuf> {
    templates_dir(s).ok_or_else(|| bad(cmd, "there is no data folder to keep templates in"))
}

fn name_p(p: &Value, cmd: &str) -> Result<String> {
    let n = str_p(p, "name").ok_or_else(|| bad(cmd, "need `name` (the template's name)"))?;
    safe_name(n).map_err(|e| bad(cmd, e))
}

fn nodes_p(p: &Value, cmd: &str) -> Result<Vec<NodeId>> {
    let a = p.get("nodes").and_then(Value::as_array).ok_or_else(|| bad(cmd, "need `nodes` (the node ids to save)"))?;
    if a.len() > filmcraft_project::dag::MAX_NODES.saturating_mul(4) {
        return Err(bad(cmd, "too many node ids"));
    }
    a.iter().map(|v| v.as_u64().ok_or_else(|| bad(cmd, "`nodes` must be node ids"))).collect()
}

fn at_p(p: &Value, cmd: &str) -> Result<Option<[f32; 2]>> {
    let Some(v) = p.get("at") else { return Ok(None) };
    let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| bad(cmd, "`at` must be [x, y]"))?;
    let f = |i: usize| a.get(i).and_then(Value::as_f64).filter(|f| f.is_finite() && f.abs() <= 1e6).map(|f| f as f32);
    match (f(0), f(1)) {
        (Some(x), Some(y)) => Ok(Some([x, y])),
        _ => Err(bad(cmd, "`at` must be two finite numbers")),
    }
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

/// One template file's bytes: a regular file (symbolic links are refused, so nothing outside the
/// folder is read), at most [`MAX_BYTES`] whatever its metadata says.
fn read_bytes(path: &Path) -> std::result::Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() {
        return Err("it is a link to another file, which a template can't be".into());
    }
    if !meta.is_file() {
        return Err("it is not a file".into());
    }
    if meta.len() > MAX_BYTES as u64 {
        return Err("the template file is larger than 4 MiB".into());
    }
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    f.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("the template file is larger than 4 MiB".into());
    }
    Ok(bytes)
}

/// Is there a regular file (not a link or a folder) at `path`?
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
}

/// The names of the template files in `dir` that match `name` ignoring case (some file systems
/// treat them as one file), sorted.
fn same_name_ignoring_case(dir: &Path, name: &str) -> Vec<String> {
    let want = name.to_lowercase();
    let suffix = format!(".{EXTENSION}");
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .take(MAX_SCANNED)
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            let stem = n.strip_suffix(&suffix)?;
            (stem.to_lowercase() == want).then(|| stem.to_string())
        })
        .collect();
    out.sort();
    out
}

/// Are `a` and `b` the same file (a case-insensitive file system)?
fn is_same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// Is this a save's temporary file name (`.<name>.fctemplate.tmp-…`)?
fn is_temp_name(n: &str) -> bool {
    n.starts_with('.') && n.contains(&format!(".{EXTENSION}.tmp-"))
}

/// Writes `bytes` to `path` atomically: a new temporary file in the same folder (a unique name,
/// created exclusively), synced, then renamed over `path`. The temporary file goes on any error.
fn write_atomic(dir: &Path, name: &str, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.{EXTENSION}.tmp-{}-{n}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.template.save";
    let name = name_p(p, CMD)?;
    let nodes = nodes_p(p, CMD)?;
    let replace = bool_p(p, "replace").unwrap_or(false);
    // (C4.8: `group` saves nodes from inside that group)
    let c = comp_at(s, p, CMD)?.1;
    let made = CompTemplate::from_selection(c, &nodes, &name).map_err(|e| bad(CMD, e.to_string()))?;
    let bytes = made.template.to_bytes().map_err(|e| EngineError::Other(e.to_string()))?;
    if bytes.len() > MAX_BYTES {
        return Err(bad(CMD, "those nodes make a template larger than 4 MiB"));
    }
    let dir = dir_or_err(s, CMD)?;
    let path = path_of(&dir, &name);
    // a name that differs only in case is the same file on some systems
    let variants = same_name_ignoring_case(&dir, &name);
    let exact = std::fs::symlink_metadata(&path).is_ok();
    if let Some(old) = variants.iter().find(|v| **v == name).or(variants.first()) {
        if !replace {
            return Err(bad(CMD, format!("a template called \u{201c}{old}\u{201d} already exists (pass `replace: true` to overwrite it)")));
        }
        if !is_regular_file(&path_of(&dir, old)) {
            return Err(bad(CMD, format!("\u{201c}{old}\u{201d} in the templates folder is not a template file; it was left alone")));
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("could not create the templates folder {}: {e}", dir.display())))?;
    write_atomic(&dir, &name, &path, &bytes).map_err(|e| EngineError::Other(format!("could not save the template to {}: {e}", path.display())))?;
    // replaced under another spelling: the old file goes only when it is the one other variant
    // (an exact file was overwritten in place; several variants are left for the user)
    if !exact && let [old] = variants.as_slice() {
        let old = path_of(&dir, old);
        if is_regular_file(&old) && !is_same_file(&old, &path) {
            let _ = std::fs::remove_file(old);
        }
    }
    let count = made.template.nodes.len();
    let mut message = format!("Saved template \u{201c}{name}\u{201d} ({})", plural(count, "node"));
    if made.dropped_media_in > 0 {
        message.push_str(&format!(
            "; left out {} that read{} a project item",
            plural(made.dropped_media_in, "Media In node"),
            if made.dropped_media_in == 1 { "s" } else { "" }
        ));
    }
    if made.dropped_media_out > 0 {
        message.push_str("; Media Out is never saved");
    }
    for w in &made.warnings {
        message.push_str("; ");
        message.push_str(w);
    }
    s.toast(message.clone());
    Ok(json!({
        "name": name,
        "path": path.to_string_lossy(),
        "nodes": count,
        "edges": made.template.edges.len(),
        "droppedMediaIn": made.dropped_media_in,
        "droppedMediaOut": made.dropped_media_out,
        "warnings": made.warnings,
        "message": message,
    }))
}

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let names_only = bool_p(p, "namesOnly").unwrap_or(false);
    let Some(dir) = templates_dir(s) else { return Ok(json!({"templates": []})) };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(json!({"templates": [], "folder": dir.to_string_lossy()})) };
    let now = SystemTime::now();
    let suffix = format!(".{EXTENSION}");
    let mut found: Vec<(String, PathBuf, Option<SystemTime>)> = Vec::new();
    for entry in entries.flatten().take(MAX_SCANNED) {
        let Ok(file_name) = entry.file_name().into_string() else { continue };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        // a save's temporary file left by a crash: swept once it is an hour old
        if is_temp_name(&file_name) {
            let stale = meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age > STALE_TEMP);
            if stale && meta.is_file() {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        let Some(stem) = file_name.strip_suffix(&suffix) else { continue };
        if found.len() >= MAX_LISTED || !meta.is_file() || safe_name(stem).ok().as_deref() != Some(stem) {
            continue;
        }
        found.push((stem.to_string(), path, meta.modified().ok()));
    }
    found.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    // names only (the canvas menu): no file is read
    if names_only {
        let templates: Vec<Value> = found.into_iter().map(|(name, ..)| json!({"name": name})).collect();
        return Ok(json!({"templates": templates, "folder": dir.to_string_lossy()}));
    }
    // parsed under a running byte budget; past it, names are listed with unknown counts
    let mut budget = LIST_BUDGET;
    let mut templates = Vec::with_capacity(found.len());
    for (name, path, modified) in found {
        let modified = modified.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
        if budget == 0 {
            templates.push(json!({"name": name, "nodes": Value::Null, "edges": Value::Null, "modified": modified}));
            continue;
        }
        // unreadable or damaged files are skipped
        let Ok(bytes) = read_bytes(&path) else { continue };
        budget = budget.saturating_sub(bytes.len() as u64);
        let Ok(t) = CompTemplate::parse(&bytes) else { continue };
        templates.push(json!({"name": name, "nodes": t.nodes.len(), "edges": t.edges.len(), "modified": modified}));
    }
    Ok(json!({"templates": templates, "folder": dir.to_string_lossy()}))
}

fn insert(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.template.insert";
    let name = name_p(p, CMD)?;
    let at = at_p(p, CMD)?;
    let id = target(s, p, CMD)?;
    let dir = dir_or_err(s, CMD)?;
    let path = path_of(&dir, &name);
    if std::fs::symlink_metadata(&path).is_err() {
        return Err(bad(CMD, format!("there is no template called \u{201c}{name}\u{201d}")));
    }
    let bytes = read_bytes(&path).map_err(|e| bad(CMD, format!("template \u{201c}{name}\u{201d}: {e}")))?;
    let mut t = CompTemplate::parse(&bytes).map_err(|e| bad(CMD, format!("template \u{201c}{name}\u{201d}: {e}")))?;
    // C4.8 review: Group Inputs (saved from inside a group) read a group's inputs, so the comp's
    // own level leaves them out (with their connections), and says so
    let mut left_out = Vec::new();
    if crate::comp::group_p(p, CMD)?.is_empty() {
        let gi = |n: &filmcraft_project::comp::CompNode| {
            matches!(n.kind, filmcraft_project::comp::CompKind::MediaIn { source: filmcraft_project::comp::MediaInSource::GroupInput { .. }, .. })
        };
        let dropped: Vec<NodeId> = t.nodes.iter().filter(|n| gi(n)).map(|n| n.id).collect();
        if !dropped.is_empty() {
            t.nodes.retain(|n| !gi(n));
            t.edges.retain(|e| !dropped.contains(&e.from.node) && !dropped.contains(&e.to.node));
            if t.nodes.is_empty() {
                return Err(bad(
                    CMD,
                    format!("template \u{201c}{name}\u{201d} holds only Group Inputs, which work inside a group only; open a group and insert it there"),
                ));
            }
            left_out.push(format!(
                "left out {} Group Input node(s): they read a group's inputs and work inside a group only (open a group to insert them)",
                dropped.len()
            ));
        }
    }
    // default: below the comp's lowest node, lined up with its leftmost
    let at = match at {
        Some(a) => a,
        None => {
            let c = comp_at(s, p, CMD)?.1;
            let x = c.nodes.iter().map(|n| n.pos[0]).filter(|v| v.is_finite()).fold(f32::INFINITY, f32::min);
            let y = c.nodes.iter().map(|n| n.pos[1]).filter(|v| v.is_finite()).fold(f32::NEG_INFINITY, f32::max);
            if x.is_finite() && y.is_finite() { [x, (y + 160.0).min(1e6)] } else { [0.0, 0.0] }
        }
    };
    // (C4.8: `group` inserts inside that group)
    let p = match p.get("group") {
        Some(g) => json!({"comp": id.0, "group": g}),
        None => json!({"comp": id.0}),
    };
    let mut done = edit_comp(s, &p, CMD, "Insert Template", None, |c| t.insert(c, at).map_err(|e| bad(CMD, e.to_string())))?;
    done.warnings.extend(left_out);
    for w in &done.warnings {
        s.toast(w.clone());
    }
    Ok(json!({"name": name, "nodes": done.nodes, "warnings": done.warnings}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "comp.template.delete";
    let name = name_p(p, CMD)?;
    let dir = dir_or_err(s, CMD)?;
    let path = path_of(&dir, &name);
    if !is_regular_file(&path) {
        return Err(bad(CMD, format!("there is no template called \u{201c}{name}\u{201d}")));
    }
    std::fs::remove_file(&path).map_err(|e| EngineError::Other(format!("could not delete the template {}: {e}", path.display())))?;
    s.toast(format!("Deleted template \u{201c}{name}\u{201d}"));
    Ok(json!({"deleted": name}))
}

#[cfg(test)]
#[path = "comp_template_tests.rs"]
mod tests;
