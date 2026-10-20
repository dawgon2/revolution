//! Sending and receiving project packages through a remote location (STORAGE-PLAN S2 item 6,
//! FORK-PLAN §1a.82).
//!
//! `remote.sendProject` packages the project as one `.revpkg` (S1.5) into the outbox under the
//! data folder, then uploads it to the location as a background job (progress, Cancel, three
//! tries on a dropped connection; every backend's upload is all-or-nothing, so the server never
//! shows half a package). `remote.receiveProject` downloads a `.revpkg` in 8 MiB ranges into
//! `<data>/remote/inbox/<name>.part`: a download that stops part-way continues from where it got
//! to on the next try. Then the package is checked against its manifest and imported, relinked
//! (`projects.importPackage`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use filmcraft_export::{Progress, Report};
use filmcraft_remote::{Location, RemoteError, RemotePath, RemoteStore};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{Job, Result, Session};

/// Bytes per download range.
#[cfg(not(test))]
const CHUNK: u64 = 8 << 20;
#[cfg(test)]
const CHUNK: u64 = 4096;
/// The largest package received (a project with its media; bounded so a hostile server can't
/// fill the disk without end).
pub const MAX_PACKAGE_BYTES: u64 = 4 << 40;
/// Tries per upload / download range on a transient error.
const TRIES: u32 = 3;

fn data_folder(s: &Session) -> PathBuf {
    s.prefs_path.as_deref().and_then(Path::parent).map(Path::to_path_buf).or_else(crate::autosave::data_dir).unwrap_or_else(std::env::temp_dir)
}

fn location(cmd: &str, s: &Session, p: &Value) -> Result<Location> {
    let id = str_p(p, "id").ok_or_else(|| bad(cmd, "need `id`"))?;
    s.prefs.remote.find(id).cloned().ok_or_else(|| bad(cmd, format!("no remote location {id:?}")))
}

fn new_job(s: &mut Session, label: String) -> Job {
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0).saturating_add(1);
    let job = Job { id, label, progress: Arc::new(Progress::default()), result: Arc::new(Mutex::new(None)) };
    s.jobs.push(job.clone());
    job
}

fn finish(job: &Job, r: std::result::Result<Report, String>) {
    if let Err(e) = &r {
        *job.progress.error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e.clone());
    }
    *job.result.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
    job.progress.finished.store(true, Ordering::Relaxed);
}

fn status(job: &Job, text: &str) {
    *job.progress.status.lock().unwrap_or_else(PoisonError::into_inner) = text.into();
}

fn report(path: &str, bytes: u64) -> Report {
    Report { path: path.into(), frames: 0, seconds: 0.0, bytes, render_fps: 0.0, extra_files: Vec::new() }
}

/// Waits (bounded by the job's own end) for the packaging job to finish.
fn wait_for(job: &Job, cancel: &Progress) -> std::result::Result<(), String> {
    loop {
        if let Some(r) = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            return r.map(|_| ());
        }
        if cancel.cancel.load(Ordering::Relaxed) {
            job.progress.cancel.store(true, Ordering::Relaxed);
            return Err("cancelled".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Uploads `file` to `to`, retrying transient failures. Progress in bytes.
fn upload(store: &dyn RemoteStore, file: &Path, to: &RemotePath, prog: &Progress) -> std::result::Result<u64, String> {
    let size = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    prog.total.store(size, Ordering::Relaxed);
    if let Some(dir) = to.parent() {
        store.make_dir(&dir).map_err(|e| e.to_string())?;
    }
    let mut last = String::new();
    for _ in 0..TRIES {
        let mut f = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let r = store.write(to, &mut f, size, &mut |done| {
            prog.done.store(done, Ordering::Relaxed);
            !prog.cancel.load(Ordering::Relaxed)
        });
        match r {
            Ok(()) => {
                let e = store.stat(to).map_err(|e| e.to_string())?;
                if e.size != size {
                    return Err(format!("the server has {} bytes instead of {size}", e.size));
                }
                return Ok(size);
            }
            Err(e) if e.is_transient() => last = e.to_string(),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("the connection kept dropping: {last}"))
}

/// `remote.sendProject`: packages a project as one `.revpkg` and uploads it to a location.
fn send(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.sendProject";
    let loc = location(CMD, s, p)?;
    let project = str_p(p, "path").ok_or_else(|| bad(CMD, "need `path` (the project file)"))?.to_string();
    let folder = RemotePath::parse(str_p(p, "folder").unwrap_or("")).map_err(|e| bad(CMD, e.to_string()))?;
    let name = Path::new(&project).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let remote = folder.join(&format!("{name}.{}", crate::package::EXT)).map_err(|e| bad(CMD, e.to_string()))?;
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let outbox = data_folder(s).join("remote").join("outbox").join(format!("{nonce}"));
    std::fs::create_dir_all(&outbox).map_err(|e| bad(CMD, format!("{}: {e}", outbox.display())))?;
    let wait = bool_p(p, "wait").unwrap_or(false);
    let pk = s.execute(
        "projects.package",
        json!({"path": project, "destination": outbox.to_string_lossy(), "format": "file", "proxies": bool_p(p, "proxies").unwrap_or(false), "trim": bool_p(p, "trim").unwrap_or(false), "wait": wait}),
    );
    let pk = match pk {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&outbox);
            return Err(e);
        }
    };
    let file = PathBuf::from(pk["package"].as_str().unwrap_or_default());
    let packaging = pk["job"].as_u64().and_then(|id| s.jobs.iter().find(|j| j.id == id).cloned());
    let job = new_job(s, format!("Send {name} to {}", loc.name));
    let run = {
        let (job, loc, remote, outbox) = (job.clone(), loc.clone(), remote.clone(), outbox.clone());
        move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Report, String> {
                if let Some(pj) = &packaging {
                    status(&job, "Packaging…");
                    wait_for(pj, &job.progress)?;
                }
                status(&job, "Uploading…");
                let store = crate::remote::connect(&loc)?;
                let bytes = upload(&*store, &file, &remote, &job.progress)?;
                Ok(report(remote.as_str(), bytes))
            }))
            .unwrap_or_else(|_| Err("the upload stopped unexpectedly".into()));
            // the package was only made to be sent
            let _ = std::fs::remove_dir_all(&outbox);
            finish(&job, r);
        }
    };
    if wait {
        run();
        let res = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone();
        return match res {
            Some(Ok(r)) => Ok(json!({"sent": r.path, "bytes": r.bytes, "job": job.id})),
            Some(Err(e)) => Err(bad(CMD, e)),
            None => Err(bad(CMD, "the upload did not finish")),
        };
    }
    std::thread::spawn(run);
    Ok(json!({"job": job.id, "to": remote.as_str()}))
}

/// Downloads `path` into `part` from its current length on (resume), in [`CHUNK`] ranges.
fn download(store: &dyn RemoteStore, path: &RemotePath, part: &Path, prog: &Progress) -> std::result::Result<u64, String> {
    let entry = store.stat(path).map_err(|e| e.to_string())?;
    if entry.is_dir {
        return Err(format!("{path} is a folder"));
    }
    if entry.size > MAX_PACKAGE_BYTES {
        return Err(format!("{path} is larger than {MAX_PACKAGE_BYTES} bytes"));
    }
    let size = entry.size;
    prog.total.store(size, Ordering::Relaxed);
    let mut have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if have > size {
        // a different file was here: start again
        let _ = std::fs::remove_file(part);
        have = 0;
    }
    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut tries = 0u32;
    while have < size {
        if prog.cancel.load(Ordering::Relaxed) {
            return Err("cancelled (the part downloaded so far is kept: receive again to continue)".into());
        }
        let want = CHUNK.min(size - have);
        match store.read_range(path, have, want) {
            Ok(bytes) if bytes.is_empty() => return Err(format!("the server's copy ended at {have} of {size} bytes")),
            Ok(bytes) => {
                let take = bytes.get(..usize::try_from(want).unwrap_or(usize::MAX).min(bytes.len())).unwrap_or_default();
                out.write_all(take).map_err(|e| format!("{}: {e}", part.display()))?;
                have = have.saturating_add(take.len() as u64);
                prog.done.store(have, Ordering::Relaxed);
                tries = 0;
            }
            Err(e) if e.is_transient() && tries + 1 < TRIES => tries += 1,
            Err(RemoteError::Network(e)) => return Err(format!("the connection dropped at {have} of {size} bytes ({e}): receive again to continue")),
            Err(e) => return Err(e.to_string()),
        }
    }
    out.sync_all().map_err(|e| e.to_string())?;
    Ok(size)
}

/// `remote.receiveProject`: downloads a `.revpkg` from a location (resuming a stopped download)
/// and, with `wait`, imports it at once; otherwise the reply's job downloads it and
/// `localPackage` is then imported with `projects.importPackage`.
fn receive(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.receiveProject";
    let loc = location(CMD, s, p)?;
    let path = RemotePath::parse(str_p(p, "path").unwrap_or("")).map_err(|e| bad(CMD, e.to_string()))?;
    if !crate::package::is_package_file(Path::new(path.name())) {
        return Err(bad(CMD, format!("{path} is not a .{} package", crate::package::EXT)));
    }
    let inbox = data_folder(s).join("remote").join("inbox").join(&loc.id);
    std::fs::create_dir_all(&inbox).map_err(|e| bad(CMD, format!("{}: {e}", inbox.display())))?;
    let part = inbox.join(format!("{}.part", path.name()));
    let done = inbox.join(path.name());
    let job = new_job(s, format!("Receive {} from {}", path.name(), loc.name));
    let run = {
        let (job, loc, path, part, done) = (job.clone(), loc.clone(), path.clone(), part.clone(), done.clone());
        move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Report, String> {
                status(&job, "Downloading…");
                let store = crate::remote::connect(&loc)?;
                let bytes = download(&*store, &path, &part, &job.progress)?;
                std::fs::rename(&part, &done).map_err(|e| format!("{}: {e}", done.display()))?;
                Ok(report(&done.to_string_lossy(), bytes))
            }))
            .unwrap_or_else(|_| Err("the download stopped unexpectedly".into()));
            finish(&job, r);
        }
    };
    if !bool_p(p, "wait").unwrap_or(false) {
        std::thread::spawn(run);
        return Ok(json!({"job": job.id, "localPackage": done.to_string_lossy()}));
    }
    run();
    let res = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone();
    match res {
        Some(Ok(_)) => {}
        Some(Err(e)) => return Err(bad(CMD, e)),
        None => return Err(bad(CMD, "the download did not finish")),
    }
    let mut q = json!({"path": done.to_string_lossy(), "wait": true});
    if let Some(f) = p.get("folder") {
        q["folder"] = f.clone();
    }
    let r = s.execute("projects.importPackage", q)?;
    // imported (checked against its manifest and unpacked): the download isn't needed any more
    let _ = std::fs::remove_file(&done);
    Ok(json!({"imported": r, "job": job.id}))
}

/// The most files one `remote.import` takes.
const MAX_IMPORT_FILES: usize = 10_000;

/// Where media imported from a location is kept: `<media cache>/Remote Media/<location>/<path>`.
/// Not part of the network-copies cache, so it is never evicted while a project uses it.
fn remote_media_dir(s: &Session, loc: &str) -> PathBuf {
    s.media_cache_dir().unwrap_or_else(|| data_folder(s).join("Media Cache")).join("Remote Media").join(loc)
}

/// `remote.import`: downloads media files from a location into the local media cache (resuming
/// stopped downloads; a complete copy of the same size is reused) and, with `wait`, imports
/// them into the project. Without `wait` the reply's job downloads them and `localPaths` are then
/// imported with `file.import`.
fn import(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.import";
    let loc = location(CMD, s, p)?;
    let list = p.get("paths").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `paths` (files on the location)"))?;
    if list.is_empty() || list.len() > MAX_IMPORT_FILES {
        return Err(bad(CMD, format!("give 1 to {MAX_IMPORT_FILES} files")));
    }
    let base = remote_media_dir(s, &loc.id);
    let mut files = Vec::with_capacity(list.len());
    for v in list {
        let path = RemotePath::parse(v.as_str().unwrap_or("")).map_err(|e| bad(CMD, e.to_string()))?;
        if path.is_root() {
            return Err(bad(CMD, "a path names the location's folder, not a file"));
        }
        let mut local = base.clone();
        for seg in path.segments() {
            local.push(seg);
        }
        files.push((path, local));
    }
    let job = new_job(s, format!("Import {} file(s) from {}", files.len(), loc.name));
    let locals: Vec<String> = files.iter().map(|(_, l)| l.to_string_lossy().into_owned()).collect();
    let run = {
        let (job, loc) = (job.clone(), loc.clone());
        move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::result::Result<Report, String> {
                let store = crate::remote::connect(&loc)?;
                let mut total = 0u64;
                for (i, (path, local)) in files.iter().enumerate() {
                    status(&job, &format!("Downloading {} ({} of {})…", path.name(), i + 1, files.len()));
                    let size = store.stat(path).map_err(|e| format!("{path}: {e}"))?.size;
                    if std::fs::metadata(local).is_ok_and(|m| m.len() == size) {
                        total = total.saturating_add(size);
                        continue;
                    }
                    if let Some(dir) = local.parent() {
                        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    }
                    let part = local.with_file_name(format!("{}.part", path.name()));
                    let bytes = download(&*store, path, &part, &job.progress)?;
                    std::fs::rename(&part, local).map_err(|e| format!("{}: {e}", local.display()))?;
                    total = total.saturating_add(bytes);
                }
                Ok(report(&loc.name, total))
            }))
            .unwrap_or_else(|_| Err("the download stopped unexpectedly".into()));
            finish(&job, r);
        }
    };
    if !bool_p(p, "wait").unwrap_or(false) {
        std::thread::spawn(run);
        return Ok(json!({"job": job.id, "localPaths": locals}));
    }
    run();
    let res = job.result.lock().unwrap_or_else(PoisonError::into_inner).clone();
    match res {
        Some(Ok(_)) => {}
        Some(Err(e)) => return Err(bad(CMD, e)),
        None => return Err(bad(CMD, "the download did not finish")),
    }
    let r = s.execute("file.import", json!({"paths": locals}))?;
    Ok(json!({"imported": r, "localPaths": locals, "job": job.id}))
}

type RunFn = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: RunFn) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("remote.sendProject", "Send Project…", r#"{"id":str,"path":str,"folder":str?,"proxies":bool?,"trim":bool?,"wait":bool?}"#, send),
        spec("remote.import", "Import from Remote Location", r#"{"id":str,"paths":[str],"wait":bool?}"#, import),
        spec("remote.receiveProject", "Receive Project…", r#"{"id":str,"path":str,"folder":str?,"wait":bool?}"#, receive),
    ]
}

#[cfg(test)]
#[path = "remote_transfer_tests.rs"]
mod tests;
