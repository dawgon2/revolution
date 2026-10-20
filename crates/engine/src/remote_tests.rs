use serde_json::json;

use super::*;
use filmcraft_remote::secrets::SecretStore;

fn session_with_prefs(dir: &std::path::Path) -> Session {
    Session { prefs_path: Some(dir.join("preferences.json")), ..Default::default() }
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("rev-remote-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn add_list_edit_remove_keeps_secrets_out_of_preferences() {
    let keyring = test_stores::keyring();
    let dir = tmp("add");
    let mut s = session_with_prefs(&dir);
    let v = s
        .execute("remote.add", json!({"kind": "webDav", "name": "Home NAS", "url": "https://nas.local/dav/", "user": "ana", "secret": "pa55-SECRET-word"}))
        .unwrap();
    let id = v["location"]["id"].as_str().unwrap().to_string();
    assert_eq!(id, "home-nas");
    assert_eq!(keyring.get("revolution-remote:home-nas").unwrap().as_deref(), Some("pa55-SECRET-word"));
    // a second with the same name gets its own id
    let v2 =
        s.execute("remote.add", json!({"kind": "s3", "name": "Home NAS", "url": "http://minio:9000", "root": "films", "secret": "S3-SECRET-key"})).unwrap();
    assert_eq!(v2["location"]["id"], "home-nas-2");
    // edit without `secret` keeps the stored one
    s.execute("remote.add", json!({"id": "home-nas", "name": "NAS"})).unwrap();
    assert_eq!(s.prefs.remote.find("home-nas").unwrap().name, "NAS");
    assert_eq!(s.prefs.remote.find("home-nas").unwrap().url, "https://nas.local/dav/");
    assert_eq!(keyring.get("revolution-remote:home-nas").unwrap().as_deref(), Some("pa55-SECRET-word"));

    let listed = s.execute("remote.list", json!({})).unwrap().to_string();
    let saved = std::fs::read_to_string(dir.join("preferences.json")).unwrap();
    for text in [&listed, &saved] {
        assert!(!text.contains("SECRET"), "a secret leaked: {text}");
    }
    assert!(saved.contains("nas.local"));

    let r = s.execute("remote.remove", json!({"id": "home-nas"})).unwrap();
    assert_eq!(r["secretRemoved"], true);
    assert_eq!(keyring.get("revolution-remote:home-nas").unwrap(), None);
    assert!(s.prefs.remote.find("home-nas").is_none());
    assert!(s.execute("remote.remove", json!({"id": "home-nas"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_locations_are_refused() {
    let mut s = Session::default();
    for p in [
        json!({"kind": "webDav", "name": "x", "url": "ftp://host"}),
        json!({"kind": "sftp", "name": "x", "url": "https://host"}),
        json!({"kind": "webDav", "name": "", "url": "https://host"}),
        json!({"kind": "webDav", "name": "x", "url": "https://"}),
        json!({"kind": "s3", "name": "x", "url": "https://s3.host"}),
        json!({"kind": "webDav", "name": "x", "url": "https://h", "root": "a/../b"}),
        json!({"kind": "webDav", "name": "x\u{7}y", "url": "https://h"}),
        json!({"kind": "gopher", "name": "x", "url": "https://h"}),
        json!({"name": "x", "url": "https://h"}),
        json!({"kind": "webDav", "name": "x", "url": "https://h", "id": "../evil"}),
    ] {
        assert!(s.execute("remote.add", p.clone()).is_err(), "accepted {p}");
    }
    assert!(s.prefs.remote.locations.is_empty());
}

#[test]
fn test_and_browse_a_location() {
    let mut s = Session::default();
    s.execute("remote.add", json!({"kind": "webDav", "name": "Mem", "url": "https://mem.test", "id": "mem-browse"})).unwrap();
    let store = test_stores::put("mem-browse");
    for (p, bytes) in [("b.mov", &b"12"[..]), ("A folder/x.rev", b"x"), (".hidden", b"h")] {
        store.write(&RemotePath::parse(p).unwrap(), &mut &bytes[..], bytes.len() as u64, &mut |_| true).unwrap();
    }
    let t = s.execute("remote.test", json!({"id": "mem-browse"})).unwrap();
    assert_eq!((t["ok"].as_bool(), t["safeLocking"].as_bool(), t["entries"].as_u64()), (Some(true), Some(true), Some(3)));
    assert_eq!(store.files().len(), 3, "the lock probe is removed again");
    let v = s.execute("remote.browse", json!({"id": "mem-browse"})).unwrap();
    let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(names, ["A folder", "b.mov"], "folders first, hidden left out");
    assert!(s.execute("remote.browse", json!({"id": "mem-browse", "path": "../x"})).is_err());
    assert!(s.execute("remote.browse", json!({"id": "nope"})).is_err());
    // a location without a backend reports the reason instead of failing the command
    s.execute("remote.add", json!({"kind": "sftp", "name": "Box", "url": "sftp://box.invalid", "id": "no-store"})).unwrap();
    assert_eq!(s.execute("remote.test", json!({"id": "no-store"})).unwrap()["ok"], false);
}

/// A server that ignores "create only if absent" (nginx's WebDAV module does): the check says
/// locking isn't safe.
#[test]
fn check_reports_servers_that_ignore_conditional_create() {
    struct Ignores(filmcraft_remote::MemoryStore);
    impl RemoteStore for Ignores {
        fn list(&self, d: &RemotePath) -> filmcraft_remote::Result<Vec<filmcraft_remote::Entry>> {
            self.0.list(d)
        }
        fn stat(&self, p: &RemotePath) -> filmcraft_remote::Result<filmcraft_remote::Entry> {
            self.0.stat(p)
        }
        fn read_range(&self, p: &RemotePath, o: u64, l: u64) -> filmcraft_remote::Result<Vec<u8>> {
            self.0.read_range(p, o, l)
        }
        fn write(&self, p: &RemotePath, d: &mut dyn std::io::Read, n: u64, pr: filmcraft_remote::Progress) -> filmcraft_remote::Result<()> {
            self.0.write(p, d, n, pr)
        }
        fn create_new(&self, p: &RemotePath, b: &[u8]) -> filmcraft_remote::Result<()> {
            self.0.write(p, &mut &b[..], b.len() as u64, &mut |_| true)
        }
        fn rename(&self, f: &RemotePath, t: &RemotePath) -> filmcraft_remote::Result<()> {
            self.0.rename(f, t)
        }
        fn delete(&self, p: &RemotePath) -> filmcraft_remote::Result<()> {
            self.0.delete(p)
        }
        fn make_dir(&self, d: &RemotePath) -> filmcraft_remote::Result<()> {
            self.0.make_dir(d)
        }
    }
    let bad = Ignores(filmcraft_remote::MemoryStore::new());
    let r = check_store(&bad).unwrap();
    assert!(!r.safe_locking);
    assert!(bad.0.files().is_empty());
    assert!(check_store(&filmcraft_remote::MemoryStore::new()).unwrap().safe_locking);
}
