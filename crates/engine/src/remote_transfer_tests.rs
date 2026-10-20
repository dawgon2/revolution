use serde_json::json;

use super::*;
use crate::remote::test_stores;

fn p(s: &str) -> RemotePath {
    RemotePath::parse(s).unwrap()
}

/// Send a project with its media to a location, then receive it on "another computer": the
/// download stops part-way and the next receive continues from there; a dropped range is
/// retried; the received project is relinked to its unpacked media.
#[test]
fn send_and_receive_a_project_with_a_resumed_download() {
    use crate::media_test_util::{make_movie, session_with};
    let dir = std::env::temp_dir().join(format!("rev-remote-send-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::OceanSunset, 64, 36, 4);
    let (mut s, _, _) = session_with(&[&movie]);
    std::fs::create_dir_all(dir.join("a")).unwrap();
    s.prefs_path = Some(dir.join("a").join("preferences.json"));
    let store = test_stores::put("send-srv");
    s.execute("remote.add", json!({"kind": "webDav", "name": "Server", "url": "https://srv.test", "id": "send-srv"})).unwrap();
    let proj = dir.join("Reel.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let r = s.execute("remote.sendProject", json!({"id": "send-srv", "path": proj.to_string_lossy(), "folder": "inbox/ana", "wait": true})).unwrap();
    assert_eq!(r["sent"], "inbox/ana/Reel.revpkg", "{r}");
    let size = store.stat(&p("inbox/ana/Reel.revpkg")).unwrap().size;
    assert!(size > 3 * CHUNK, "big enough to need several ranges ({size})");
    assert!(!dir.join("a").join("remote").join("outbox").read_dir().unwrap().any(|e| e.unwrap().path().join("Reel.revpkg").exists()), "outbox cleaned");

    // computer b: a download that stopped after two ranges is on disk already
    let mut b = Session { prefs_path: Some(dir.join("b").join("preferences.json")), ..Default::default() };
    std::fs::create_dir_all(dir.join("b")).unwrap();
    b.prefs.general.projects_folder = Some(dir.join("b").join("Projects"));
    std::fs::create_dir_all(dir.join("b").join("Projects")).unwrap();
    b.execute("remote.add", json!({"kind": "webDav", "name": "Server", "url": "https://srv.test", "id": "send-srv"})).unwrap();
    let inbox = dir.join("b").join("remote").join("inbox").join("send-srv");
    std::fs::create_dir_all(&inbox).unwrap();
    let head = store.read_range(&p("inbox/ana/Reel.revpkg"), 0, 2 * CHUNK).unwrap();
    std::fs::write(inbox.join("Reel.revpkg.part"), &head).unwrap();
    store.drop_connection_after(1);
    let r = b.execute("remote.receiveProject", json!({"id": "send-srv", "path": "inbox/ana/Reel.revpkg", "wait": true})).unwrap();
    let job = b.jobs.iter().find(|j| j.id == r["job"].as_u64().unwrap()).unwrap();
    assert_eq!(job.progress.total.load(Ordering::Relaxed), size);
    let to = PathBuf::from(r["imported"]["path"].as_str().unwrap());
    let mut t = Session::default();
    t.execute("file.open", json!({"path": to.join("Reel.fcproj").to_string_lossy()})).unwrap();
    assert!(t.project.items.values().filter_map(|i| i.as_media()).all(|m| match &m.media {
        filmcraft_project::MediaRef::File { path } => path.starts_with(&*to.to_string_lossy()) && Path::new(path).is_file(),
        _ => true,
    }));
    assert!(!inbox.join("Reel.revpkg").exists() && !inbox.join("Reel.revpkg.part").exists(), "inbox cleaned after import");

    // not a package, a folder, a missing file
    for path in ["inbox/ana", "notes.txt", "missing.revpkg", "../x.revpkg"] {
        assert!(b.execute("remote.receiveProject", json!({"id": "send-srv", "path": path, "wait": true})).is_err(), "{path}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Media imported from a location lands in the media cache's Remote Media folder (resuming a
/// stopped download, reusing a complete copy) and in the project.
#[test]
fn import_media_from_a_location() {
    use crate::media_test_util::make_movie;
    let dir = std::env::temp_dir().join(format!("rev-remote-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let movie = dir.join("clip.mov");
    make_movie(&movie, filmcraft_media::DemoScene::OceanSunset, 64, 36, 4);
    let bytes = std::fs::read(&movie).unwrap();
    let store = test_stores::put("imp-srv");
    store.write(&p("shoot/day 1/clip.mov"), &mut &bytes[..], bytes.len() as u64, &mut |_| true).unwrap();
    let mut s = Session { prefs_path: Some(dir.join("preferences.json")), ..Default::default() };
    s.prefs.media_cache.location = dir.join("cache").to_string_lossy().into_owned();
    s.execute("remote.add", json!({"kind": "webDav", "name": "Server", "url": "https://srv.test", "id": "imp-srv"})).unwrap();
    let local = remote_media_dir(&s, "imp-srv").join("shoot").join("day 1").join("clip.mov");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(local.with_file_name("clip.mov.part"), &bytes[..bytes.len() / 3]).unwrap();
    let r = s.execute("remote.import", json!({"id": "imp-srv", "paths": ["shoot/day 1/clip.mov"], "wait": true})).unwrap();
    assert_eq!(std::fs::read(&local).unwrap(), bytes, "resumed download is byte-identical");
    assert_eq!(r["localPaths"][0], json!(local.to_string_lossy()));
    assert!(
        s.project
            .items
            .values()
            .filter_map(|i| i.as_media())
            .any(|m| matches!(&m.media, filmcraft_project::MediaRef::File { path } if Path::new(path) == local))
    );
    // a second import reuses the copy (the server's file is gone: nothing is downloaded)
    store.drop_connection_after(0);
    assert!(s.execute("remote.import", json!({"id": "imp-srv", "paths": ["shoot/day 1/clip.mov"], "wait": true})).is_ok());
    for bad in [json!([]), json!(["../x.mov"]), json!([""]), json!("x")] {
        assert!(s.execute("remote.import", json!({"id": "imp-srv", "paths": bad, "wait": true})).is_err());
    }
    let _ = std::fs::remove_dir_all(&dir);
}
