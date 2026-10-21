//! `nearby.*` (STORAGE-PLAN S2b items 4–5): two sessions on one machine send a project over
//! loopback (discovery off, typed address); Decline and a wrong code end cleanly with nothing
//! imported.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::{make_movie, session_with};

fn tmp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let d = std::env::temp_dir().join(format!("filmcraft-nearby-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A sender session with a saved project (one short clip) and a receiver session, each with
/// its own data folder and projects folder.
fn pair(dir: &Path) -> (Session, PathBuf, Session) {
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::Dunes, 64, 36, 4);
    let (mut a, _, _) = session_with(&[&movie]);
    a.prefs_path = Some(dir.join("a-data").join("preferences.json"));
    let proj = dir.join("Trip.fcproj");
    a.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let mut b = Session { prefs_path: Some(dir.join("b-data").join("preferences.json")), ..Session::default() };
    b.prefs.general.projects_folder = Some(dir.join("B Projects"));
    std::fs::create_dir_all(dir.join("B Projects")).unwrap();
    (a, proj, b)
}

fn status(b: &mut Session) -> Value {
    b.execute("nearby.status", json!({})).unwrap()
}

fn wait_offer(b: &mut Session) -> Value {
    for _ in 0..600 {
        let st = status(b);
        if !st["offer"].is_null() {
            return st;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no offer arrived: {}", status(b));
}

fn job_result(s: &Session, id: u64) -> Option<Result<String, String>> {
    let j = s.jobs.iter().find(|j| j.id == id)?;
    let r = j.result.lock().unwrap().clone()?;
    Some(r.map(|r| r.path))
}

fn wait_job(s: &Session, id: u64) -> Result<String, String> {
    for _ in 0..1500 {
        if let Some(r) = job_result(s, id) {
            return r;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("job {id} did not finish");
}

#[test]
fn a_project_goes_to_the_other_session_after_accept() {
    let dir = tmp("accept");
    let (mut a, proj, mut b) = pair(&dir);
    let r = b.execute("nearby.receive", json!({"discovery": false})).unwrap();
    let code = r["code"].as_str().unwrap().to_string();
    let port = r["port"].as_u64().unwrap();
    let rjob = r["job"].as_u64().unwrap();
    assert!(b.execute("nearby.accept", json!({})).is_err(), "nothing to accept yet");
    let s = a.execute("nearby.send", json!({"code": code, "path": proj.to_string_lossy(), "address": format!("127.0.0.1:{port}")})).unwrap();
    let sjob = s["job"].as_u64().unwrap();
    let st = wait_offer(&mut b);
    assert_eq!(st["offer"]["package"], "Trip.revpkg", "{st}");
    assert!(st["offer"]["bytes"].as_u64().unwrap() > 0);
    assert!(std::fs::read_dir(dir.join("b-data/remote/inbox/nearby")).unwrap().next().is_none(), "nothing written before Accept");
    b.execute("nearby.accept", json!({})).unwrap();
    let got = wait_job(&b, rjob).expect("received");
    wait_job(&a, sjob).expect("sent");
    assert!(got.ends_with(".revpkg") && Path::new(&got).is_file(), "{got}");
    let imp = b.execute("projects.importPackage", json!({"path": got, "wait": true})).unwrap();
    let to = PathBuf::from(imp["path"].as_str().unwrap());
    assert!(to.join("Trip.fcproj").is_file(), "imported: {imp}");
    assert!(!dir.join("a-data/remote/outbox").read_dir().is_ok_and(|mut d| d.next().is_some()), "the sender's package is cleaned up");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn decline_and_a_wrong_code_end_cleanly() {
    let dir = tmp("decline");
    let (mut a, proj, mut b) = pair(&dir);
    // wrong code: the sender is told, the receiver gets a new code and keeps waiting
    let r = b.execute("nearby.receive", json!({"discovery": false})).unwrap();
    let (code, port, rjob) = (r["code"].as_str().unwrap().to_string(), r["port"].as_u64().unwrap(), r["job"].as_u64().unwrap());
    let channel = code.split('-').next().unwrap().to_string();
    let w = filmcraft_remote::nearby::words::WORDS;
    let pick = w.iter().find(|x| !code.contains(*x)).unwrap();
    let wrong = format!("{channel}-{pick}-{pick}-{pick}");
    let e = a.execute("nearby.send", json!({"code": wrong, "path": proj.to_string_lossy(), "address": format!("127.0.0.1:{port}"), "wait": true}));
    assert!(e.is_err_and(|e| e.to_string().contains("didn't match")), "wrong code refused");
    for _ in 0..200 {
        if status(&mut b)["code"] != json!(code) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let st = status(&mut b);
    assert_ne!(st["code"], json!(code), "a new code after a wrong one");
    assert!(st["note"].as_str().unwrap().contains("wrong code"));
    assert!(job_result(&b, rjob).is_none(), "still receiving");

    // decline: both sides end, nothing written
    let (code, port) = (st["code"].as_str().unwrap().to_string(), st["port"].as_u64().unwrap());
    let s = a.execute("nearby.send", json!({"code": code, "path": proj.to_string_lossy(), "address": format!("127.0.0.1:{port}")})).unwrap();
    wait_offer(&mut b);
    b.execute("nearby.decline", json!({})).unwrap();
    assert!(wait_job(&b, rjob).is_err_and(|e| e.contains("declined")));
    assert!(wait_job(&a, s["job"].as_u64().unwrap()).is_err_and(|e| e.contains("declined")));
    assert!(std::fs::read_dir(dir.join("b-data/remote/inbox/nearby")).unwrap().next().is_none());
    // stop with nothing running is harmless
    assert_eq!(b.execute("nearby.stop", json!({})).unwrap()["stopped"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_parameters_are_refused() {
    let mut s = Session::default();
    assert!(s.execute("nearby.send", json!({"code": "not a code", "path": "/x.fcproj"})).is_err());
    assert!(s.execute("nearby.send", json!({"code": "7-velvet-velvet-velvet"})).is_err());
    assert!(s.execute("nearby.decline", json!({})).is_err());
    assert!(!status(&mut s)["active"].as_bool().unwrap());
}

/// Scripts: `nearby.receive` gives the code at once, `nearby.wait` accepts and imports.
#[test]
fn a_script_receives_with_wait_and_accept() {
    let dir = tmp("wait");
    let (mut a, proj, mut b) = pair(&dir);
    assert!(b.execute("nearby.wait", json!({})).is_err(), "nothing running");
    let r = b.execute("nearby.receive", json!({"discovery": false})).unwrap();
    let (code, port) = (r["code"].as_str().unwrap().to_string(), r["port"].as_u64().unwrap());
    a.execute("nearby.send", json!({"code": code, "path": proj.to_string_lossy(), "address": format!("127.0.0.1:{port}")})).unwrap();
    let w = b.execute("nearby.wait", json!({"accept": true, "timeoutS": 60})).unwrap();
    let to = PathBuf::from(w["imported"]["path"].as_str().unwrap());
    assert!(to.join("Trip.fcproj").is_file(), "{w}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings ▸ Remote Locations ▸ the fixed port is used when the command doesn't name one.
#[test]
fn the_fixed_port_setting_is_used() {
    let dir = tmp("port");
    let mut b = Session { prefs_path: Some(dir.join("preferences.json")), ..Session::default() };
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    b.prefs.remote.nearby_port = free;
    let r = b.execute("nearby.receive", json!({"discovery": false})).unwrap();
    assert_eq!(r["port"].as_u64(), Some(u64::from(free)));
    b.execute("nearby.stop", json!({})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
