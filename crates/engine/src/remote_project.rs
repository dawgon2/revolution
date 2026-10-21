//! Projects on a remote location (STORAGE-PLAN S2 item 4): open / save / Save As without
//! mounting anything.
//!
//! Opening downloads the project file into a local working copy
//! (`<data dir>/remote/projects/<location>/<path>`; autosave and the local backup keep working on
//! it) and takes the project's lock file ([`filmcraft_remote::lock`]). Someone else's lock opens
//! the project read-only unless the user takes it over. Saving writes the local copy, then
//! uploads it; the copy on the server before it is kept in `.<name>.versions/` (the newest
//! [`KEEP_VERSIONS`]). If another computer took the lock or changed the file meanwhile, the save
//! becomes a **conflict copy** next to it instead of overwriting their work.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use filmcraft_remote::lock::{self, Acquire, LockInfo};
use filmcraft_remote::{RemoteError, RemotePath, RemoteStore};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

/// Server copies kept per project in `.<name>.versions/`.
pub const KEEP_VERSIONS: usize = 10;

/// The largest project file opened from a server.
pub const MAX_PROJECT_BYTES: u64 = 256 << 20;

/// The project this session has open from a remote location.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProject {
    /// The location's id.
    pub location: String,
    pub path: RemotePath,
    /// The local working copy.
    pub local: PathBuf,
    /// Our lock (`None`: opened read-only because someone else holds it).
    pub lock: Option<LockInfo>,
    /// Who held it when we opened read-only.
    pub held_by: Option<LockInfo>,
    /// The server's version tag and size of the file as we last saw it (a different one at save
    /// time means another computer changed it).
    pub etag: Option<String>,
    pub size: u64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// This app session's lock id (random per process).
fn session_id() -> String {
    #[cfg(test)]
    if let Some(id) = tests::FAKE_SESSION.with(|f| f.borrow().clone()) {
        return id;
    }
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        format!("{:x}-{:x}", nanos, std::process::id())
    })
    .clone()
}

pub(crate) fn machine_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(64).collect())
        .unwrap_or_else(|| "this computer".into())
}

fn user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .map(|s| s.chars().filter(|c| !c.is_control()).take(64).collect())
        .unwrap_or_else(|_| "someone".into())
}

fn me() -> LockInfo {
    LockInfo { holder: user_name(), machine: machine_name(), session: session_id(), since: now_secs() }
}

/// `YYYY-MM-DD HHMM` (UTC) for file names.
fn stamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil from days (Howard Hinnant's algorithm, public domain)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}{:02}", rem / 3600, (rem % 3600) / 60)
}

fn remote_err(cmd: &str, e: RemoteError) -> EngineError {
    bad(cmd, e.to_string())
}

/// The local working copy of `path` on location `id`: under the data folder the session's
/// preferences live in (the app's data folder).
fn local_copy(s: &Session, id: &str, path: &RemotePath) -> PathBuf {
    let data = s.prefs_path.as_deref().and_then(Path::parent).map(Path::to_path_buf).or_else(crate::autosave::data_dir).unwrap_or_else(std::env::temp_dir);
    let mut p = data.join("remote").join("projects").join(id);
    // RemotePath segments are checked: no `..`, no separators
    for seg in path.segments() {
        p.push(seg);
    }
    p
}

fn download(store: &dyn RemoteStore, path: &RemotePath, to: &Path) -> std::result::Result<(), String> {
    let bytes = store.read_all(path, MAX_PROJECT_BYTES).map_err(|e| e.to_string())?;
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = to.with_extension("download");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, to).map_err(|e| format!("{}: {e}", to.display()))
}

fn upload(store: &dyn RemoteStore, from: &Path, to: &RemotePath) -> std::result::Result<(), RemoteError> {
    let size = std::fs::metadata(from)?.len();
    let mut f = std::fs::File::open(from)?;
    store.write(to, &mut f, size, &mut |_| true)
}

fn versions_dir(path: &RemotePath) -> std::result::Result<RemotePath, RemoteError> {
    let parent = path.parent().unwrap_or_default();
    parent.join(&format!(".{}.versions", path.name()))
}

/// Moves the server's current copy into the versions folder and drops the oldest beyond
/// [`KEEP_VERSIONS`]. Returns where it went (to put it back if the upload fails).
fn keep_version(store: &dyn RemoteStore, path: &RemotePath) -> std::result::Result<Option<RemotePath>, RemoteError> {
    match store.stat(path) {
        Ok(e) if !e.is_dir => {}
        Ok(_) => return Err(RemoteError::AlreadyExists(format!("{path} is a folder"))),
        Err(RemoteError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    }
    let dir = versions_dir(path)?;
    store.make_dir(&dir)?;
    let mut name = stamp(now_secs()).replace(' ', "-");
    let ext = path.name().rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
    if !ext.is_empty() {
        name = format!("{name}.{ext}");
    }
    let mut to = dir.join(&name)?;
    let mut n = 2u32;
    while store.stat(&to).is_ok() && n < 100 {
        to = dir.join(&format!("{n}-{name}"))?;
        n += 1;
    }
    store.rename(path, &to)?;
    if let Ok(mut old) = store.list(&dir) {
        old.retain(|e| !e.is_dir);
        old.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.path.cmp(&b.path)));
        let extra = old.len().saturating_sub(KEEP_VERSIONS);
        for e in old.iter().take(extra) {
            let _ = store.delete(&e.path);
        }
    }
    Ok(Some(to))
}

/// A free conflict-copy name next to `path`: `<stem> (conflict from <machine> <date>).<ext>`.
fn conflict_path(store: &dyn RemoteStore, path: &RemotePath, machine: &str) -> std::result::Result<RemotePath, RemoteError> {
    let parent = path.parent().unwrap_or_default();
    let (stem, ext) = match path.name().rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (path.name().to_string(), String::new()),
    };
    let machine: String = machine.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == ' ').take(32).collect();
    let base = format!("{stem} (conflict from {machine} {})", stamp(now_secs()));
    for n in 1..100u32 {
        let name = if n == 1 { format!("{base}{ext}") } else { format!("{base} {n}{ext}") };
        let p = parent.join(&name)?;
        if matches!(store.stat(&p), Err(RemoteError::NotFound(_))) {
            return Ok(p);
        }
    }
    Err(RemoteError::AlreadyExists("too many conflict copies".into()))
}

fn location(cmd: &str, s: &Session, id: &str) -> Result<filmcraft_remote::Location> {
    s.prefs.remote.find(id).cloned().ok_or_else(|| bad(cmd, format!("no remote location {id:?}")))
}

fn open_store(cmd: &str, s: &Session, id: &str) -> Result<Box<dyn RemoteStore>> {
    let loc = location(cmd, s, id)?;
    crate::remote::connect(&loc).map_err(|e| bad(cmd, e))
}

fn path_param(cmd: &str, p: &Value, key: &str) -> Result<RemotePath> {
    let raw = str_p(p, key).ok_or_else(|| bad(cmd, format!("need `{key}`")))?;
    let path = RemotePath::parse(raw).map_err(|e| bad(cmd, e.to_string()))?;
    if path.is_root() {
        return Err(bad(cmd, format!("`{key}` must name a project file")));
    }
    if !crate::projects::is_project_file(Path::new(path.name())) {
        return Err(bad(cmd, format!("{path} is not a project file")));
    }
    Ok(path)
}

/// `remote.openProject`: downloads and opens a project from a location, taking its lock.
fn open(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.openProject";
    let id = str_p(p, "id").ok_or_else(|| bad(CMD, "need `id`"))?.to_string();
    let path = path_param(CMD, p, "path")?;
    let store = open_store(CMD, s, &id)?;
    let entry = store.stat(&path).map_err(|e| remote_err(CMD, e))?;
    if entry.is_dir {
        return Err(bad(CMD, format!("{path} is a folder")));
    }
    let mine = me();
    let (lock, held_by) = if bool_p(p, "readOnly").unwrap_or(false) {
        (None, lock::read(&*store, &path).map_err(|e| remote_err(CMD, e))?)
    } else {
        let take = bool_p(p, "takeOver").unwrap_or(false);
        let r = if take { lock::take_over(&*store, &path, &mine) } else { lock::acquire(&*store, &path, &mine) };
        match r.map_err(|e| remote_err(CMD, e))? {
            Acquire::Acquired => (Some(mine.clone()), None),
            Acquire::HeldBy(h) => (None, Some(h)),
        }
    };
    let local = local_copy(s, &id, &path);
    if let Err(e) = download(&*store, &path, &local) {
        if lock.is_some() {
            let _ = lock::release(&*store, &path, &mine);
        }
        return Err(bad(CMD, e));
    }
    let mut q = json!({"path": local.to_string_lossy()});
    if let Some(d) = p.get("discard") {
        q["discard"] = d.clone();
    }
    if let Err(e) = s.execute("file.open", q) {
        if lock.is_some() {
            let _ = lock::release(&*store, &path, &mine);
        }
        return Err(e);
    }
    // a project opened before (another remote one) gives its lock back
    if let Some(old) = s.remote_project.take() {
        release_binding(s, &old);
    }
    let rp = RemoteProject { location: id, path, local: local.clone(), lock, held_by: held_by.clone(), etag: entry.etag, size: entry.size };
    s.remote_project = Some(rp);
    Ok(
        json!({"local": local.to_string_lossy(), "readOnly": held_by.is_some() || s.remote_project.as_ref().is_some_and(|r| r.lock.is_none()), "heldBy": held_by}),
    )
}

fn release_binding(s: &Session, rp: &RemoteProject) {
    let (Some(mine), Ok(loc)) = (&rp.lock, location("remote.closeProject", s, &rp.location)) else { return };
    if let Ok(store) = crate::remote::connect(&loc) {
        let _ = lock::release(&*store, &rp.path, mine);
    }
}

/// Uploads the saved local copy to `rp.path` (or a conflict copy). Returns the reply.
fn upload_project(store: &dyn RemoteStore, rp: &mut RemoteProject) -> std::result::Result<Value, RemoteError> {
    let Some(mine) = rp.lock.clone() else {
        return Err(RemoteError::Forbidden("this project was opened read-only".into()));
    };
    let ours = lock::still_ours(store, &rp.path, &mine)?;
    let changed = match store.stat(&rp.path) {
        Ok(e) => e.etag.is_some() && rp.etag.is_some() && e.etag != rp.etag || (rp.etag.is_none() && e.size != rp.size),
        Err(RemoteError::NotFound(_)) => false,
        Err(e) => return Err(e),
    };
    if !ours || changed {
        let to = conflict_path(store, &rp.path, &mine.machine)?;
        upload(store, &rp.local, &to)?;
        let why = if ours { "the file was changed on the server by someone else" } else { "another computer took over the project" };
        return Ok(json!({"conflict": true, "savedAs": to.as_str(), "reason": why}));
    }
    let kept = keep_version(store, &rp.path)?;
    if let Err(e) = upload(store, &rp.local, &rp.path) {
        if let Some(v) = &kept {
            // put the server's copy back where it was
            let _ = store.rename(v, &rp.path);
        }
        return Err(e);
    }
    let e = store.stat(&rp.path)?;
    rp.etag = e.etag;
    rp.size = e.size;
    Ok(json!({"conflict": false, "path": rp.path.as_str(), "versionKept": kept.map(|v| v.as_str().to_string())}))
}

/// `remote.saveProject`: saves the open remote project (locally, then to the server).
fn save(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.saveProject";
    let mut rp = s.remote_project.clone().ok_or_else(|| bad(CMD, "no project from a remote location is open"))?;
    if rp.lock.is_none() {
        let who = rp.held_by.as_ref().map(|h| format!("{} on {}", h.holder, h.machine)).unwrap_or_else(|| "someone else".into());
        return Err(bad(CMD, format!("opened read-only ({who} has it open): use Save As to keep your changes")));
    }
    s.execute("file.save", json!({}))?;
    let _ = p;
    let store = open_store(CMD, s, &rp.location)?;
    let r = upload_project(&*store, &mut rp).map_err(|e| bad(CMD, format!("saved on this computer, but not on the server: {e}")))?;
    s.remote_project = Some(rp);
    Ok(r)
}

/// `remote.saveProjectAs`: saves the current project to `path` on location `id` (refuses an
/// existing file unless `overwrite`) and continues there with its lock.
fn save_as(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.saveProjectAs";
    let id = str_p(p, "id").ok_or_else(|| bad(CMD, "need `id`"))?.to_string();
    let path = path_param(CMD, p, "path")?;
    let store = open_store(CMD, s, &id)?;
    let exists = match store.stat(&path) {
        Ok(_) => true,
        Err(RemoteError::NotFound(_)) => false,
        Err(e) => return Err(remote_err(CMD, e)),
    };
    if exists && !bool_p(p, "overwrite").unwrap_or(false) {
        return Err(bad(CMD, format!("{path} already exists (pass overwrite to replace it)")));
    }
    let mine = me();
    match lock::acquire(&*store, &path, &mine).map_err(|e| remote_err(CMD, e))? {
        Acquire::Acquired => {}
        Acquire::HeldBy(h) => return Err(bad(CMD, format!("{} on {} has {path} open", h.holder, h.machine))),
    }
    let local = local_copy(s, &id, &path);
    let r = (|| {
        if let Some(dir) = local.parent() {
            std::fs::create_dir_all(dir).map_err(|e| bad(CMD, format!("{}: {e}", dir.display())))?;
        }
        s.execute("file.saveAs", json!({"path": local.to_string_lossy()}))?;
        if exists {
            keep_version(&*store, &path).map_err(|e| remote_err(CMD, e))?;
        }
        upload(&*store, &local, &path).map_err(|e| bad(CMD, format!("saved on this computer, but not on the server: {e}")))?;
        store.stat(&path).map_err(|e| remote_err(CMD, e))
    })();
    let entry = match r {
        Ok(e) => e,
        Err(e) => {
            let _ = lock::release(&*store, &path, &mine);
            return Err(e);
        }
    };
    if let Some(old) = s.remote_project.take() {
        release_binding(s, &old);
    }
    s.remote_project =
        Some(RemoteProject { location: id, path: path.clone(), local: local.clone(), lock: Some(mine), held_by: None, etag: entry.etag, size: entry.size });
    Ok(json!({"path": path.as_str(), "local": local.to_string_lossy()}))
}

/// `remote.closeProject`: gives the lock back (the project stays open locally, unbound).
fn close(s: &mut Session, _: &Value) -> Result<Value> {
    let Some(rp) = s.remote_project.take() else { return Ok(json!({"closed": false})) };
    release_binding(s, &rp);
    Ok(json!({"closed": true, "path": rp.path.as_str()}))
}

/// `remote.unlock`: removes a project's lock file whoever holds it (a computer that crashed or
/// went offline with the project open).
fn unlock(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.unlock";
    let id = str_p(p, "id").ok_or_else(|| bad(CMD, "need `id`"))?.to_string();
    let path = path_param(CMD, p, "path")?;
    let store = open_store(CMD, s, &id)?;
    let held = lock::read(&*store, &path).map_err(|e| remote_err(CMD, e))?;
    if held.is_some() {
        match store.delete(&lock::lock_path(&path).map_err(|e| remote_err(CMD, e))?) {
            Ok(()) | Err(RemoteError::NotFound(_)) => {}
            Err(e) => return Err(remote_err(CMD, e)),
        }
    }
    if let Some(rp) = s.remote_project.as_mut().filter(|r| r.location == id && r.path == path) {
        rp.lock = None;
    }
    Ok(json!({"unlocked": held.is_some(), "heldBy": held}))
}

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(match &s.remote_project {
        Some(rp) => json!({"open": true, "project": rp, "readOnly": rp.lock.is_none()}),
        None => json!({"open": false}),
    })
}

type RunFn = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: RunFn) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("remote.openProject", "Open Project from Remote Location", r#"{"id":str,"path":str,"takeOver":bool?,"readOnly":bool?,"discard":bool?}"#, open),
        spec("remote.saveProject", "Save Project to Remote Location", "{}", save),
        spec("remote.saveProjectAs", "Save Project to Remote Location As", r#"{"id":str,"path":str,"overwrite":bool?}"#, save_as),
        spec("remote.closeProject", "Release Remote Project", "{}", close),
        spec("remote.unlock", "Unlock Remote Project", r#"{"id":str,"path":str}"#, unlock),
        spec("remote.projectStatus", "Remote Project Status", "{}", status),
    ]
}

#[cfg(test)]
#[path = "remote_project_tests.rs"]
mod tests;
