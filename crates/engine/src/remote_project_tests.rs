use std::cell::RefCell;

use serde_json::json;

use super::*;
use crate::remote::test_stores;

thread_local! {
    /// Lock session id for this test thread (two "machines" in one test).
    pub(super) static FAKE_SESSION: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn as_machine(name: &str) {
    FAKE_SESSION.with(|f| *f.borrow_mut() = Some(name.into()));
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rev-remote-proj-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A session whose data folder is `dir/<who>`, with the remote location `id` added.
fn machine(dir: &Path, who: &str, id: &str) -> Session {
    let data = dir.join(who);
    std::fs::create_dir_all(&data).unwrap();
    let mut s = Session { prefs_path: Some(data.join("preferences.json")), ..Default::default() };
    s.execute("remote.add", json!({"kind": "webDav", "name": "Server", "url": "https://srv.test", "id": id})).unwrap();
    s
}

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

#[test]
fn two_machines_lock_take_over_and_conflict_copies() {
    let dir = tmp("two");
    let store = test_stores::put("proj-srv");
    as_machine("a");
    let mut a = machine(&dir, "a", "proj-srv");
    a.execute("file.saveAs", json!({"path": dir.join("start.fcproj").to_string_lossy()})).unwrap();
    let r = a.execute("remote.saveProjectAs", json!({"id": "proj-srv", "path": "films/cut.fcproj"})).unwrap();
    assert_eq!(r["path"], "films/cut.fcproj");
    assert!(store.stat(&p("films/cut.fcproj")).is_ok());
    assert!(lock::read(&*store, &p("films/cut.fcproj")).unwrap().is_some_and(|l| l.session == "a"));
    assert!(a.execute("remote.saveProjectAs", json!({"id": "proj-srv", "path": "films/cut.fcproj"})).is_err(), "no silent overwrite");

    // machine b: the project is held by a → read-only, saving refused
    as_machine("b");
    let mut b = machine(&dir, "b", "proj-srv");
    let r = b.execute("remote.openProject", json!({"id": "proj-srv", "path": "films/cut.fcproj"})).unwrap();
    assert_eq!(r["readOnly"], true);
    assert_eq!(r["heldBy"]["session"], "a");
    let err = b.execute("remote.saveProject", json!({})).unwrap_err().to_string();
    assert!(err.contains("read-only"), "{err}");

    // b takes over and saves: the server's copy before goes to the versions folder
    let r = b.execute("remote.openProject", json!({"id": "proj-srv", "path": "films/cut.fcproj", "takeOver": true, "discard": true})).unwrap();
    assert_eq!(r["readOnly"], false);
    b.execute("file.newBin", json!({"name": "B's cut"})).unwrap();
    let r = b.execute("remote.saveProject", json!({})).unwrap();
    assert_eq!(r["conflict"], false, "{r}");
    assert!(r["versionKept"].as_str().unwrap().starts_with("films/.cut.fcproj.versions/"));
    let b_bytes = store.read_all(&p("films/cut.fcproj"), 1 << 30).unwrap();

    // a saves without the lock: a conflict copy, b's file untouched
    as_machine("a");
    let r = a.execute("remote.saveProject", json!({})).unwrap();
    assert_eq!(r["conflict"], true, "{r}");
    let copy = r["savedAs"].as_str().unwrap().to_string();
    assert!(copy.starts_with("films/cut (conflict from ") && copy.ends_with(".fcproj"), "{copy}");
    assert_eq!(store.read_all(&p("films/cut.fcproj"), 1 << 30).unwrap(), b_bytes);
    // a's release leaves b's lock alone
    a.execute("remote.closeProject", json!({})).unwrap();
    assert!(lock::read(&*store, &p("films/cut.fcproj")).unwrap().is_some_and(|l| l.session == "b"));

    // b's computer "crashed": unlock clears it
    let r = a.execute("remote.unlock", json!({"id": "proj-srv", "path": "films/cut.fcproj"})).unwrap();
    assert_eq!(r["unlocked"], true);
    assert_eq!(lock::read(&*store, &p("films/cut.fcproj")).unwrap(), None);
    FAKE_SESSION.with(|f| *f.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn versions_are_capped_and_a_failed_upload_puts_the_old_copy_back() {
    let dir = tmp("versions");
    let store = test_stores::put("ver-srv");
    as_machine("v");
    let mut s = machine(&dir, "v", "ver-srv");
    s.execute("file.saveAs", json!({"path": dir.join("v.fcproj").to_string_lossy()})).unwrap();
    s.execute("remote.saveProjectAs", json!({"id": "ver-srv", "path": "v.fcproj"})).unwrap();
    for i in 0..(KEEP_VERSIONS + 3) {
        store.set_clock(i64::try_from(i).unwrap());
        s.execute("file.newBin", json!({"name": format!("s{i}")})).unwrap();
        s.execute("remote.saveProject", json!({})).unwrap();
    }
    let kept = store.list(&p(".v.fcproj.versions")).unwrap();
    assert_eq!(kept.len(), KEEP_VERSIONS);
    let before = store.read_all(&p("v.fcproj"), 1 << 30).unwrap();
    store.drop_connection_after(1);
    s.execute("file.newBin", json!({"name": "lost"})).unwrap();
    let err = s.execute("remote.saveProject", json!({})).unwrap_err().to_string();
    assert!(err.contains("not on the server"), "{err}");
    assert_eq!(store.read_all(&p("v.fcproj"), 1 << 30).unwrap(), before, "the server keeps the last good copy");
    FAKE_SESSION.with(|f| *f.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_paths_and_missing_things_are_refused() {
    let dir = tmp("hostile");
    test_stores::put("h-srv");
    let mut s = machine(&dir, "h", "h-srv");
    for (cmd, params) in [
        ("remote.openProject", json!({"id": "h-srv", "path": "../x.fcproj"})),
        ("remote.openProject", json!({"id": "h-srv", "path": "notes.txt"})),
        ("remote.openProject", json!({"id": "h-srv", "path": ""})),
        ("remote.openProject", json!({"id": "h-srv", "path": "missing.fcproj"})),
        ("remote.openProject", json!({"id": "nope", "path": "a.fcproj"})),
        ("remote.saveProjectAs", json!({"id": "h-srv", "path": "a/\u{0}.fcproj"})),
        ("remote.saveProject", json!({})),
    ] {
        assert!(s.execute(cmd, params.clone()).is_err(), "{cmd} {params}");
    }
    assert!(s.remote_project.is_none());
    assert_eq!(stamp(0), "1970-01-01 0000");
    assert_eq!(stamp(1_760_000_000), "2025-10-09 0853");
    let _ = std::fs::remove_dir_all(&dir);
}

/// S2 acceptance: a location's password never lands in any file the session writes (preferences,
/// the working copy, the project on the server, kept versions, lock files).
#[test]
fn no_credential_in_any_written_file() {
    let _ = test_stores::keyring();
    const SECRET: &str = "Zz-very-SECRET-pass-9";
    let dir = tmp("creds");
    let store = test_stores::put("cred-srv");
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut s = Session { prefs_path: Some(data.join("preferences.json")), ..Default::default() };
    s.execute("remote.add", json!({"kind": "webDav", "name": "Server", "url": "https://srv.test", "user": "ana", "secret": SECRET, "id": "cred-srv"})).unwrap();
    s.execute("file.saveAs", json!({"path": dir.join("p.fcproj").to_string_lossy()})).unwrap();
    s.execute("remote.saveProjectAs", json!({"id": "cred-srv", "path": "p.fcproj"})).unwrap();
    s.execute("file.newBin", json!({})).unwrap();
    s.execute("remote.saveProject", json!({})).unwrap();
    let mut replies = String::new();
    for cmd in ["remote.list", "remote.projectStatus", "prefs.get"] {
        replies.push_str(&s.execute(cmd, json!({})).map(|v| v.to_string()).unwrap_or_default());
    }
    assert!(!replies.contains(SECRET));
    fn scan(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() { scan(&p, out) } else { out.push(p) }
        }
    }
    let mut files = Vec::new();
    scan(&dir, &mut files);
    assert!(files.len() >= 3, "{files:?}");
    for f in files {
        let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).into_owned();
        assert!(!text.contains(SECRET), "the password is in {}", f.display());
    }
    for f in store.files() {
        let bytes = store.read_all(&f, 1 << 30).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(SECRET), "the password is in the server's {f}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
