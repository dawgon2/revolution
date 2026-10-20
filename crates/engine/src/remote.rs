//! Remote locations (STORAGE-PLAN S2): the saved servers (Settings ▸ Remote locations) and the
//! `remote.*` commands the UI, CLI and MCP share.
//!
//! A [`Location`] in the preferences never holds a secret: passwords, keys and secret keys go
//! to [`filmcraft_remote::secrets::store`] (the system keyring in the app) under
//! [`Location::secret_key`], and every reply here leaves them out.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_remote::{Kind, Location, RemotePath, RemoteStore};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

/// The most locations kept.
pub const MAX_LOCATIONS: usize = 64;

/// Settings ▸ Remote locations.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemotePrefs {
    pub locations: Vec<Location>,
    /// The user allowed keeping secrets in a file (not encrypted) when no keyring works.
    pub secrets_in_file: bool,
}

impl RemotePrefs {
    pub fn find(&self, id: &str) -> Option<&Location> {
        self.locations.iter().find(|l| l.id == id)
    }
}

/// Where secrets are kept now (for Settings), in plain words.
pub fn secret_store_name() -> String {
    filmcraft_remote::secrets::store().describe()
}

/// Applies the preferences that live outside the session (the secret store's file fallback).
/// Cheap: the UI calls it whenever preferences are applied.
pub fn apply_prefs(p: &RemotePrefs) {
    filmcraft_remote::secrets::set_file_fallback_allowed(p.secrets_in_file);
}

fn kind_p(cmd: &str, p: &Value) -> Result<Kind> {
    match str_p(p, "kind").unwrap_or("") {
        "webDav" | "webdav" => Ok(Kind::WebDav),
        "sftp" => Ok(Kind::Sftp),
        "s3" => Ok(Kind::S3),
        other => Err(bad(cmd, format!("`kind` must be webDav, sftp or s3 (not {other:?})"))),
    }
}

/// Checks a location's fields (the URL's scheme fits the kind, no control characters, sane
/// lengths). Returns a plain-language reason.
pub fn validate(loc: &Location) -> std::result::Result<(), String> {
    let fields = [("name", &loc.name), ("url", &loc.url), ("user", &loc.user), ("root", &loc.root), ("region", &loc.region)];
    for (what, v) in fields {
        if v.len() > 2048 || v.chars().any(char::is_control) {
            return Err(format!("the {what} is too long or has control characters"));
        }
    }
    if loc.name.trim().is_empty() {
        return Err("give the location a name".into());
    }
    let url = loc.url.trim().to_ascii_lowercase();
    let ok = match loc.kind {
        Kind::WebDav | Kind::S3 => url.starts_with("https://") || url.starts_with("http://"),
        Kind::Sftp => url.starts_with("sftp://") || url.starts_with("ssh://"),
    };
    if !ok {
        return Err(match loc.kind {
            Kind::WebDav | Kind::S3 => "the address must start with https:// (or http:// on a home network)".into(),
            Kind::Sftp => "the address must start with sftp://".into(),
        });
    }
    if url.split_once("://").is_none_or(|(_, rest)| rest.split('/').next().is_none_or(str::is_empty)) {
        return Err("the address needs a server name".into());
    }
    if loc.kind == Kind::S3 && loc.root.trim_matches('/').is_empty() {
        return Err("an S3 location needs a bucket in Folder (bucket or bucket/folder)".into());
    }
    if !loc.root.trim_matches('/').is_empty() {
        RemotePath::parse(&loc.root).map_err(|e| format!("Folder: {e}"))?;
    }
    Ok(())
}

/// An id from the name: lower-case letters, digits and dashes, unique among `taken`.
fn new_id(name: &str, taken: &[Location]) -> String {
    let mut base: String = name
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c == ' ' || c == '-' || c == '_' {
                Some('-')
            } else {
                None
            }
        })
        .take(40)
        .collect();
    base = base.trim_matches('-').to_string();
    if base.is_empty() {
        base = "location".into();
    }
    let mut id = base.clone();
    let mut n = 2u32;
    while taken.iter().any(|l| l.id == id) {
        id = format!("{base}-{n}");
        n = n.saturating_add(1);
    }
    id
}

fn save(s: &mut Session, f: impl FnOnce(&mut RemotePrefs)) -> Result<()> {
    let mut r = s.prefs.remote.clone();
    f(&mut r);
    if r == s.prefs.remote {
        return Ok(());
    }
    s.prefs.remote = r;
    if let Some(path) = s.prefs_path.clone() {
        s.prefs.save(&path).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    }
    Ok(())
}

fn location_json(l: &Location) -> Value {
    // never the secret: it is not in a Location at all
    json!({"id": l.id, "name": l.name, "kind": l.kind, "url": l.url, "user": l.user, "root": l.root, "region": l.region})
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let secrets = filmcraft_remote::secrets::store();
    Ok(json!({
        "locations": s.prefs.remote.locations.iter().map(location_json).collect::<Vec<_>>(),
        "secretStore": secrets.describe(),
        "secretsInFile": s.prefs.remote.secrets_in_file,
    }))
}

/// `remote.add`: adds a location, or changes the one with `id`. `secret` (password, private key
/// or S3 secret key) goes to the secret store, never to the preferences; leave it out to keep
/// the stored one.
fn add(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.add";
    let existing = str_p(p, "id").and_then(|id| s.prefs.remote.find(id)).cloned();
    let kind = if p.get("kind").is_some() {
        kind_p(CMD, p)?
    } else {
        existing.as_ref().map(|l| l.kind).ok_or_else(|| bad(CMD, "need `kind` (webDav, sftp or s3)"))?
    };
    let field = |k: &str, old: Option<&String>| str_p(p, k).map(|v| v.trim().to_string()).or_else(|| old.cloned()).unwrap_or_default();
    let mut loc = Location {
        id: String::new(),
        name: field("name", existing.as_ref().map(|l| &l.name)),
        kind,
        url: field("url", existing.as_ref().map(|l| &l.url)),
        user: field("user", existing.as_ref().map(|l| &l.user)),
        root: field("root", existing.as_ref().map(|l| &l.root)),
        region: field("region", existing.as_ref().map(|l| &l.region)),
    };
    validate(&loc).map_err(|e| bad(CMD, e))?;
    if existing.is_none() && s.prefs.remote.locations.len() >= MAX_LOCATIONS {
        return Err(bad(CMD, format!("at most {MAX_LOCATIONS} remote locations")));
    }
    loc.id = match &existing {
        Some(l) => l.id.clone(),
        None => match str_p(p, "id") {
            Some(id) if !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => id.to_string(),
            Some(_) => return Err(bad(CMD, "`id` may only use letters, digits, - and _")),
            None => new_id(&loc.name, &s.prefs.remote.locations),
        },
    };
    if let Some(secret) = str_p(p, "secret") {
        filmcraft_remote::secrets::store().set(&loc.secret_key(), secret).map_err(|e| bad(CMD, format!("couldn't store the password: {e}")))?;
    }
    let out = location_json(&loc);
    save(s, |r| match r.locations.iter_mut().find(|l| l.id == loc.id) {
        Some(slot) => *slot = loc,
        None => r.locations.push(loc),
    })?;
    Ok(json!({"location": out}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.remove";
    let id = str_p(p, "id").ok_or_else(|| bad(CMD, "need `id`"))?.to_string();
    let loc = s.prefs.remote.find(&id).cloned().ok_or_else(|| bad(CMD, format!("no remote location {id:?}")))?;
    // best effort: a keyring that refuses leaves an orphaned entry, never a kept location
    let secret_removed = filmcraft_remote::secrets::store().delete(&loc.secret_key()).is_ok();
    save(s, |r| r.locations.retain(|l| l.id != id))?;
    Ok(json!({"removed": id, "secretRemoved": secret_removed}))
}

/// Opens a connection to a saved location with its stored secret.
pub fn connect(loc: &Location) -> std::result::Result<Box<dyn RemoteStore>, String> {
    if let Some(store) = test_stores::get(&loc.id) {
        return Ok(Box::new(store));
    }
    let secret = filmcraft_remote::secrets::store().get(&loc.secret_key()).map_err(|e| e.to_string())?.unwrap_or_default();
    open_backend(loc, &secret)
}

#[cfg(feature = "remote-net")]
fn open_backend(loc: &Location, secret: &str) -> std::result::Result<Box<dyn RemoteStore>, String> {
    match loc.kind {
        Kind::WebDav => Ok(Box::new(filmcraft_remote::webdav::WebDav::new(loc, secret).map_err(|e| e.to_string())?)),
        Kind::S3 => Ok(Box::new(filmcraft_remote::s3::S3::new(loc, secret).map_err(|e| e.to_string())?)),
        Kind::Sftp => {
            // trust on first use: the server's key is remembered here and checked every time after
            let known_hosts = crate::autosave::data_dir().unwrap_or_else(std::env::temp_dir).join("remote").join("known_hosts");
            Ok(Box::new(filmcraft_remote::sftp::Sftp::new(loc, secret, &known_hosts).map_err(|e| e.to_string())?))
        }
    }
}

#[cfg(not(feature = "remote-net"))]
fn open_backend(_: &Location, _: &str) -> std::result::Result<Box<dyn RemoteStore>, String> {
    Err("this build has no network support for remote locations".into())
}

fn location_p<'a>(cmd: &str, s: &'a Session, p: &Value) -> Result<&'a Location> {
    let id = str_p(p, "id").ok_or_else(|| bad(cmd, "need `id`"))?;
    s.prefs.remote.find(id).ok_or_else(|| bad(cmd, format!("no remote location {id:?}")))
}

/// What [`check`] found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    /// Entries in the location's folder.
    pub entries: usize,
    /// The server refuses a second "create only if absent" of the same file, so project locks
    /// work. Some servers (nginx's WebDAV module, older MinIO / Garage) ignore it: then two
    /// computers could both think they hold a project.
    pub safe_locking: bool,
}

/// Connects, lists the folder and probes whether lock files are safe on this server (writes and
/// deletes one small hidden file). Blocking, bounded by the backends' timeouts; run it off the
/// UI thread.
pub fn check(loc: &Location) -> std::result::Result<CheckReport, String> {
    let store = connect(loc)?;
    check_store(&*store)
}

fn check_store(store: &dyn RemoteStore) -> std::result::Result<CheckReport, String> {
    let entries = store.list(&RemotePath::root()).map_err(|e| e.to_string())?.len();
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()) ^ u128::from(std::process::id());
    let probe = RemotePath::root().join(&format!(".revolution-lock-probe-{nonce:x}")).map_err(|e| e.to_string())?;
    store.create_new(&probe, b"probe").map_err(|e| format!("can't write to the folder: {e}"))?;
    let second = store.create_new(&probe, b"probe-2");
    let _ = store.delete(&probe);
    let safe_locking = matches!(second, Err(filmcraft_remote::RemoteError::AlreadyExists(_)));
    Ok(CheckReport { entries, safe_locking })
}

/// `remote.test`: [`check`] for a saved location. Blocking; the Settings page runs [`check`]
/// off the UI thread instead.
fn test(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.test";
    let loc = location_p(CMD, s, p)?.clone();
    Ok(match check(&loc) {
        Ok(r) => json!({"ok": true, "entries": r.entries, "safeLocking": r.safe_locking}),
        Err(e) => json!({"ok": false, "error": e}),
    })
}

/// `remote.browse`: one folder of a location (folders first, then by name).
fn browse(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "remote.browse";
    let loc = location_p(CMD, s, p)?.clone();
    let path = RemotePath::parse(str_p(p, "path").unwrap_or("")).map_err(|e| bad(CMD, e.to_string()))?;
    let store = connect(&loc).map_err(|e| bad(CMD, e))?;
    let mut entries = store.list(&path).map_err(|e| bad(CMD, e.to_string()))?;
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase())));
    if !bool_p(p, "hidden").unwrap_or(false) {
        entries.retain(|e| !e.name().starts_with('.'));
    }
    Ok(json!({"id": loc.id, "path": path.as_str(), "entries": entries}))
}

type RunFn = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, params: &'static str, run: RunFn) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        // the app opens its browser window (File ▸ Remote Locations…); headless: the list
        CommandSpec {
            id: "remote.show",
            label: "Remote Locations…",
            menu: &["File"],
            shortcut: None,
            params: r#"{"id":str?}"#,
            enabled: always,
            run: list,
            journal: false,
        },
        spec("remote.list", "List Remote Locations", "{}", list),
        spec(
            "remote.add",
            "Add Remote Location",
            r#"{"kind":"webDav|sftp|s3"?,"name":str?,"url":str?,"user":str?,"root":str?,"region":str?,"secret":str?,"id":str?}"#,
            add,
        ),
        spec("remote.remove", "Remove Remote Location", r#"{"id":str}"#, remove),
        spec("remote.test", "Test Remote Location", r#"{"id":str}"#, test),
        spec("remote.browse", "Browse Remote Location", r#"{"id":str,"path":str?,"hidden":bool?}"#, browse),
    ]
}

/// In-memory locations for tests (the engine's and the UI's): `connect` returns these by id.
/// Empty unless a test puts one in.
#[doc(hidden)]
pub mod test_stores {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};

    use filmcraft_remote::{Entry, MemoryStore, Progress, RemotePath, RemoteStore};

    static STORES: OnceLock<Mutex<HashMap<String, Arc<MemoryStore>>>> = OnceLock::new();

    /// One in-memory secret store for the whole test process (the store is process-wide and
    /// tests run in parallel).
    #[cfg(test)]
    pub(crate) fn keyring() -> Arc<filmcraft_remote::secrets::MemorySecrets> {
        static K: OnceLock<Arc<filmcraft_remote::secrets::MemorySecrets>> = OnceLock::new();
        K.get_or_init(|| {
            let k = Arc::new(filmcraft_remote::secrets::MemorySecrets::default());
            filmcraft_remote::secrets::install(k.clone());
            k
        })
        .clone()
    }

    pub fn put(id: &str) -> Arc<MemoryStore> {
        let store = Arc::new(MemoryStore::new());
        STORES.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner).insert(id.into(), store.clone());
        store
    }

    pub(super) fn get(id: &str) -> Option<Shared> {
        STORES.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned().map(Shared)
    }

    /// An `Arc<MemoryStore>` as a boxed store.
    pub(super) struct Shared(Arc<MemoryStore>);

    impl RemoteStore for Shared {
        fn list(&self, dir: &RemotePath) -> filmcraft_remote::Result<Vec<Entry>> {
            self.0.list(dir)
        }
        fn stat(&self, path: &RemotePath) -> filmcraft_remote::Result<Entry> {
            self.0.stat(path)
        }
        fn read_range(&self, path: &RemotePath, offset: u64, len: u64) -> filmcraft_remote::Result<Vec<u8>> {
            self.0.read_range(path, offset, len)
        }
        fn write(&self, path: &RemotePath, data: &mut dyn std::io::Read, size: u64, progress: Progress) -> filmcraft_remote::Result<()> {
            self.0.write(path, data, size, progress)
        }
        fn create_new(&self, path: &RemotePath, bytes: &[u8]) -> filmcraft_remote::Result<()> {
            self.0.create_new(path, bytes)
        }
        fn rename(&self, from: &RemotePath, to: &RemotePath) -> filmcraft_remote::Result<()> {
            self.0.rename(from, to)
        }
        fn delete(&self, path: &RemotePath) -> filmcraft_remote::Result<()> {
            self.0.delete(path)
        }
        fn make_dir(&self, dir: &RemotePath) -> filmcraft_remote::Result<()> {
            self.0.make_dir(dir)
        }
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
