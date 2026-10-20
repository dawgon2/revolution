//! Fork RS2: the Project Manager home screen's engine side: listing, sorting, searching,
//! summaries and their cache, hostile files, trash, rename / duplicate collisions, names that try
//! to leave the projects folder, startup choice and preferences.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::tmp_dir;
use crate::projects::{self, SortKey, StartupAction, StartupShow};
use crate::settings::Workflow;

struct Env {
    s: Session,
    dir: PathBuf,
    root: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn env(tag: &str) -> Env {
    let dir = tmp_dir(&format!("rs2-{tag}"));
    let root = dir.join("Projects");
    let mut s = Session::default();
    s.prefs.general.projects_folder = Some(root.clone());
    std::fs::create_dir_all(dir.join("data")).unwrap();
    s.prefs_path = Some(dir.join("data/preferences.json"));
    s.trash_dir = Some(dir.join("Trash"));
    Env { s, dir, root }
}

/// A saved project with one `w`×`h` sequence at `fps`.
fn save_project(s: &mut Session, path: &Path, w: u32, h: u32, fps: f64) {
    s.execute("file.newProject", json!({"name": "x"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Seq", "width": w, "height": h, "fps": fps})).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
}

fn list(s: &mut Session, p: Value) -> Value {
    s.execute("projects.list", p).unwrap()
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v[key].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect()
}

fn set_mtime(path: &Path, secs: u64) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

#[test]
fn lists_folders_and_projects_hiding_support_folders() {
    let mut e = env("list");
    save_project(&mut e.s, &e.root.join("Beta.fcproj"), 1920, 1080, 25.0);
    save_project(&mut e.s, &e.root.join("alpha.fcproj"), 3840, 2160, 23.976);
    save_project(&mut e.s, &e.root.join("Client/Gamma.fcproj"), 1280, 720, 30.0);
    std::fs::create_dir_all(e.root.join("Auto-Save")).unwrap();
    std::fs::create_dir_all(e.root.join(".hidden")).unwrap();
    std::fs::write(e.root.join("notes.txt"), "x").unwrap();
    let v = list(&mut e.s, json!({}));
    assert_eq!(names(&v, "projects"), ["alpha", "Beta"], "{v}");
    assert_eq!(names(&v, "folders"), ["Client"], "support folders (Auto-Save, previews) and dot folders are hidden");
    assert_eq!(v["folders"][0]["projects"], 1);
    let alpha = &v["projects"][0];
    assert_eq!((alpha["width"].as_u64(), alpha["height"].as_u64(), alpha["sequenceCount"].as_u64()), (Some(3840), Some(2160), Some(1)));
    assert!((alpha["fps"].as_f64().unwrap() - 23.976).abs() < 0.01);
    assert!(alpha["error"].is_null());
    // into the sub-folder
    let v = list(&mut e.s, json!({"folder": "Client"}));
    assert_eq!(names(&v, "projects"), ["Gamma"]);
    assert_eq!(v["breadcrumb"], json!(["Client"]));
    assert!(v["other"].as_array().unwrap().is_empty(), "other locations only on the top level");
}

#[test]
fn sorts_and_searches() {
    let mut e = env("sort");
    for (n, t) in [("b", 300), ("a", 100), ("c", 200)] {
        let p = e.root.join(format!("{n}.fcproj"));
        save_project(&mut e.s, &p, 640, 360, 24.0);
        set_mtime(&p, 1_700_000_000 + t);
    }
    assert_eq!(names(&list(&mut e.s, json!({"sort": "name"})), "projects"), ["a", "b", "c"]);
    assert_eq!(names(&list(&mut e.s, json!({"sort": "name", "descending": true})), "projects"), ["c", "b", "a"]);
    assert_eq!(names(&list(&mut e.s, json!({"sort": "modified"})), "projects"), ["b", "c", "a"], "newest first");
    assert_eq!(names(&list(&mut e.s, json!({"search": "B"})), "projects"), ["b"], "case-insensitive substring");
    assert!(e.s.execute("projects.list", json!({"sort": "size"})).is_err());
}

#[test]
fn garbage_files_are_cant_read_entries_not_failures() {
    let mut e = env("garbage");
    std::fs::create_dir_all(&e.root).unwrap();
    save_project(&mut e.s, &e.root.join("good.fcproj"), 1920, 1080, 25.0);
    let good = std::fs::read(e.root.join("good.fcproj")).unwrap();
    std::fs::write(e.root.join("empty.fcproj"), b"").unwrap();
    std::fs::write(e.root.join("random.fcproj"), (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect::<Vec<_>>()).unwrap();
    std::fs::write(e.root.join("truncated.fcproj"), &good[..good.len() / 2]).unwrap();
    std::fs::write(e.root.join("nonutf8.fcproj"), [0xff, 0xfe, b'{', 0x80, b'}']).unwrap();
    std::fs::write(e.root.join("foreign.fcproj"), br#"{"format":"somebody.else","schema_version":1}"#).unwrap();
    std::fs::write(e.root.join("deep.fcproj"), "[".repeat(100_000)).unwrap();
    std::fs::create_dir_all(e.root.join("dir.fcproj")).unwrap();
    // larger than the cap: never read
    std::fs::File::create(e.root.join("huge.fcproj")).unwrap().set_len(projects::MAX_PROJECT_BYTES + 1).unwrap();
    let v = list(&mut e.s, json!({}));
    let projects = v["projects"].as_array().unwrap();
    let get = |n: &str| projects.iter().find(|p| p["name"] == n).unwrap_or_else(|| panic!("{n} missing: {v}"));
    assert!(get("good")["error"].is_null());
    for bad in ["empty", "random", "truncated", "nonutf8", "foreign", "deep", "huge"] {
        assert!(get(bad)["error"].is_string(), "{bad}: {}", get(bad));
    }
    assert!(get("huge")["error"].as_str().unwrap().contains("MiB"));
    // a folder named like a project is a folder
    assert!(names(&v, "folders").contains(&"dir.fcproj".to_string()));
    // listing twice (cached) gives the same
    assert_eq!(list(&mut e.s, json!({}))["projects"], v["projects"]);
}

#[test]
fn summaries_are_cached_and_recomputed_when_the_file_changes() {
    let mut e = env("cache");
    let path = e.root.join("p.fcproj");
    save_project(&mut e.s, &path, 1920, 1080, 25.0);
    let cache = projects::cache_dir(&e.s).unwrap();
    let key = projects::cache_key(&path);
    assert!(cache.join(format!("{key}.json")).is_file(), "saving writes the summary");
    assert_eq!(e.s.poster_requests, [path.to_string_lossy().to_string()], "and asks for a poster");
    let s1 = projects::summary(Some(&cache), &path);
    assert_eq!((s1.width, s1.height), (1920, 1080));
    // a cache entry that lies but matches the file is believed (that's the point of the cache)
    let mut entry: Value = serde_json::from_slice(&std::fs::read(cache.join(format!("{key}.json"))).unwrap()).unwrap();
    entry["summary"]["width"] = json!(7);
    std::fs::write(cache.join(format!("{key}.json")), serde_json::to_vec(&entry).unwrap()).unwrap();
    assert_eq!(projects::summary(Some(&cache), &path).width, 7);
    // the file changes (another build saved it): stale, read again
    let mut other = Session::default();
    save_project(&mut other, &path, 1280, 720, 30.0);
    set_mtime(&path, 1_800_000_000);
    let s2 = projects::summary(Some(&cache), &path);
    assert_eq!((s2.width, s2.height), (1280, 720));
    // a garbage cache entry is ignored
    std::fs::write(cache.join(format!("{key}.json")), b"\x00garbage").unwrap();
    assert_eq!(projects::summary(Some(&cache), &path).width, 1280);
    // no cache at all (headless) still works
    assert_eq!(projects::summary(None, &path).width, 1280);
}

#[test]
fn posters_round_trip_downscaled_and_garbage_is_ignored() {
    let e = env("poster");
    let cache = e.dir.join("cache");
    let project = e.dir.join("p.fcproj");
    std::fs::write(&project, b"x").unwrap();
    let (w, h) = (1920u32, 1080u32);
    let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 0, 0, 255]).collect();
    projects::store_poster(&cache, &project, w, h, &rgba, Some("color")).unwrap();
    let png = cache.join(format!("{}.png", projects::cache_key(&project)));
    let (pw, ph, px) = projects::load_poster(&png).unwrap();
    assert_eq!((pw, ph), (480, 270));
    assert_eq!(px.len(), 480 * 270 * 4);
    let s = projects::summary(Some(&cache), &project);
    assert_eq!(s.page.as_deref(), Some("color"));
    assert_eq!(s.poster.as_deref(), Some(png.to_string_lossy().as_ref()));
    // malformed input is refused, garbage PNGs don't decode
    assert!(projects::store_poster(&cache, &project, 10, 10, &[0; 3], None).is_err());
    assert!(projects::downscale(0, 10, &[], 480).is_none());
    std::fs::write(&png, b"\x89PNG\r\n\x1a\nnot really").unwrap();
    assert!(projects::load_poster(&png).is_none());
}

#[test]
fn names_cannot_leave_the_folder() {
    let mut e = env("names");
    for bad in ["../x", "a/b", "a\\b", "..", ".", ".hidden", "", "   ", "x\u{0}y", "tab\there", &"n".repeat(300)] {
        assert!(projects::check_name(bad).is_err(), "{bad:?} accepted");
        assert!(e.s.execute("projects.newFolder", json!({"name": bad})).is_err(), "newFolder {bad:?}");
        assert!(e.s.execute("projects.new", json!({"name": bad})).is_err(), "new {bad:?}");
    }
    assert_eq!(projects::check_name(" Cut 1.fcproj ").unwrap(), "Cut 1");
    // a leading slash is still relative to the projects folder
    assert!(projects::resolve_folder(&e.root, "/etc").unwrap().starts_with(&e.root));
    for bad in ["..", "../..", "a/../..", "a/.git", "a/./b"] {
        assert!(e.s.execute("projects.list", json!({"folder": bad})).is_err(), "list folder {bad:?}");
        assert!(e.s.execute("projects.new", json!({"name": "ok", "folder": bad})).is_err(), "new in {bad:?}");
    }
    assert!(!e.dir.join("ok.fcproj").exists() && !Path::new("/etc/ok.fcproj").exists());
    // a project in a nested folder is fine
    e.s.execute("projects.newFolder", json!({"name": "A"})).unwrap();
    e.s.execute("projects.newFolder", json!({"name": "B", "folder": "A"})).unwrap();
    assert!(e.root.join("A/B").is_dir());
    assert!(e.s.execute("projects.newFolder", json!({"name": "A"})).is_err(), "exists");
}

#[test]
fn new_project_is_created_opened_and_never_overwrites() {
    let mut e = env("new");
    let r = e.s.execute("projects.new", json!({"name": "Holiday"})).unwrap();
    let path = e.root.join("Holiday.fcproj");
    assert_eq!(r["path"], json!(path.to_string_lossy()));
    assert!(path.is_file(), "the projects folder is made on first use");
    assert_eq!(e.s.path.as_deref(), Some(path.to_string_lossy().as_ref()));
    assert_eq!(e.s.prefs.general.recent_projects.first().map(String::as_str), Some(path.to_string_lossy().as_ref()));
    assert!(e.s.execute("projects.new", json!({"name": "Holiday"})).is_err(), "never overwrites");
    // unsaved changes are kept unless the caller says to discard them
    e.s.execute("file.newSequence", json!({"name": "S"})).unwrap();
    assert!(e.s.is_dirty());
    assert!(e.s.execute("projects.new", json!({"name": "Other"})).is_err());
    e.s.execute("projects.new", json!({"name": "Other", "discard": true})).unwrap();
    assert!(e.root.join("Other.fcproj").is_file());
    // projects.open lands back on Holiday
    let r = e.s.execute("projects.open", json!({"path": path.to_string_lossy()})).unwrap();
    assert!(r["page"].is_null(), "no page recorded yet");
    assert_eq!(e.s.path.as_deref(), Some(path.to_string_lossy().as_ref()));
    assert!(e.s.execute("projects.open", json!({"path": e.root.join("nope.fcproj").to_string_lossy()})).is_err());
}

#[test]
fn rename_and_duplicate_refuse_collisions() {
    let mut e = env("rename");
    save_project(&mut e.s, &e.root.join("One.fcproj"), 640, 360, 24.0);
    save_project(&mut e.s, &e.root.join("Two.fcproj"), 640, 360, 24.0);
    let one = e.root.join("One.fcproj");
    // Two is open: it can't be renamed or trashed
    assert!(e.s.execute("projects.rename", json!({"path": e.root.join("Two.fcproj").to_string_lossy(), "name": "Three"})).is_err());
    assert!(e.s.execute("projects.rename", json!({"path": one.to_string_lossy(), "name": "Two"})).is_err(), "collision");
    assert!(e.s.execute("projects.rename", json!({"path": one.to_string_lossy(), "name": "../Up"})).is_err());
    let r = e.s.execute("projects.rename", json!({"path": one.to_string_lossy(), "name": "Uno"})).unwrap();
    let uno = e.root.join("Uno.fcproj");
    assert_eq!(r["path"], json!(uno.to_string_lossy()));
    assert!(uno.is_file() && !one.exists());
    assert!(e.s.prefs.general.recent_projects.iter().any(|p| Path::new(p) == uno), "recents follow the rename");
    assert!(!e.s.prefs.general.recent_projects.iter().any(|p| Path::new(p) == one));
    // duplicates: "Uno copy", "Uno copy 2"; a named one can't collide
    let d1 = e.s.execute("projects.duplicate", json!({"path": uno.to_string_lossy()})).unwrap();
    let d2 = e.s.execute("projects.duplicate", json!({"path": uno.to_string_lossy()})).unwrap();
    assert_eq!((d1["name"].as_str(), d2["name"].as_str()), (Some("Uno copy"), Some("Uno copy 2")));
    assert_eq!(std::fs::read(&uno).unwrap(), std::fs::read(e.root.join("Uno copy.fcproj")).unwrap());
    assert!(e.s.execute("projects.duplicate", json!({"path": uno.to_string_lossy(), "name": "Two"})).is_err());
    // folders inside the projects folder rename too; ones outside don't
    e.s.execute("projects.newFolder", json!({"name": "F"})).unwrap();
    e.s.execute("projects.rename", json!({"path": e.root.join("F").to_string_lossy(), "name": "G"})).unwrap();
    assert!(e.root.join("G").is_dir());
    assert!(e.s.execute("projects.rename", json!({"path": e.dir.join("data").to_string_lossy(), "name": "x"})).is_err());
    assert!(e.s.execute("projects.rename", json!({"path": e.root.to_string_lossy(), "name": "x"})).is_err(), "not the root itself");
}

#[test]
fn trash_moves_into_the_trash_with_an_info_file() {
    let mut e = env("trash");
    let p = e.root.join("Gone.fcproj");
    save_project(&mut e.s, &p, 640, 360, 24.0);
    e.s.execute("file.newProject", json!({"name": "other"})).unwrap();
    let r = e.s.execute("projects.trash", json!({"path": p.to_string_lossy()})).unwrap();
    let trash = e.dir.join("Trash");
    assert!(!p.exists());
    assert_eq!(r["trashed"], json!(trash.join("files/Gone.fcproj").to_string_lossy()));
    assert!(trash.join("files/Gone.fcproj").is_file());
    let info = std::fs::read_to_string(trash.join("info/Gone.fcproj.trashinfo")).unwrap();
    assert!(info.starts_with("[Trash Info]\nPath=/"), "{info}");
    assert!(info.contains("Gone.fcproj\nDeletionDate=") && info.contains('T'), "{info}");
    assert!(!e.s.prefs.general.recent_projects.iter().any(|x| Path::new(x) == p), "gone from the recents");
    // the same name again: a second, numbered entry
    save_project(&mut e.s, &p, 640, 360, 24.0);
    e.s.execute("file.newProject", json!({"name": "other"})).unwrap();
    e.s.execute("projects.trash", json!({"path": p.to_string_lossy()})).unwrap();
    assert!(trash.join("files/Gone.2.fcproj").is_file());
    assert!(trash.join("info/Gone.2.fcproj.trashinfo").is_file());
    // the open project, a non-project file and folders outside the projects folder stay
    let open = e.root.join("Open.fcproj");
    save_project(&mut e.s, &open, 640, 360, 24.0);
    assert!(e.s.execute("projects.trash", json!({"path": open.to_string_lossy()})).is_err());
    std::fs::write(e.root.join("notes.txt"), "x").unwrap();
    assert!(e.s.execute("projects.trash", json!({"path": e.root.join("notes.txt").to_string_lossy()})).is_err());
    assert!(e.s.execute("projects.trash", json!({"path": e.dir.join("data").to_string_lossy()})).is_err());
    assert!(open.is_file() && e.dir.join("data").is_dir());
    // a folder inside the projects folder goes whole
    e.s.execute("projects.newFolder", json!({"name": "Old"})).unwrap();
    e.s.execute("projects.trash", json!({"path": e.root.join("Old").to_string_lossy()})).unwrap();
    assert!(trash.join("files/Old").is_dir() && !e.root.join("Old").exists());
}

#[test]
fn trash_info_dates_and_paths() {
    assert_eq!(projects::iso_local(0, 0), "1970-01-01T00:00:00");
    assert_eq!(projects::iso_local(1_760_000_000, 0), "2025-10-09T08:53:20");
    assert_eq!(projects::iso_local(1_760_000_000, -7 * 3600), "2025-10-09T01:53:20");
    assert_eq!(projects::iso_local(951_782_400, 0), "2000-02-29T00:00:00", "leap day");
}

#[test]
fn other_locations_list_recents_outside_the_folder() {
    let mut e = env("other");
    let elsewhere = e.dir.join("Elsewhere/Far.fcproj");
    save_project(&mut e.s, &elsewhere, 1920, 1080, 25.0);
    let inside = e.root.join("Near.fcproj");
    save_project(&mut e.s, &inside, 1920, 1080, 25.0);
    let gone = e.dir.join("Gone.fcproj");
    let mut prefs = e.s.prefs.clone();
    prefs.general.recent_projects.push(gone.to_string_lossy().into_owned());
    e.s.set_prefs(prefs).unwrap();
    let v = list(&mut e.s, json!({}));
    assert_eq!(names(&v, "projects"), ["Near"]);
    let other = v["other"].as_array().unwrap();
    assert_eq!(other.len(), 2, "{v}");
    let far = other.iter().find(|o| o["name"] == "Far").unwrap();
    assert_eq!(far["missing"], false);
    let g = other.iter().find(|o| o["name"] == "Gone").unwrap();
    assert_eq!(g["missing"], true);
    // locate it, then remove it from the list
    let moved = e.dir.join("Moved.fcproj");
    std::fs::copy(&elsewhere, &moved).unwrap();
    e.s.execute("projects.locate", json!({"path": gone.to_string_lossy(), "newPath": moved.to_string_lossy()})).unwrap();
    assert!(e.s.prefs.general.recent_projects.iter().any(|p| Path::new(p) == moved));
    let r = e.s.execute("projects.removeFromList", json!({"path": moved.to_string_lossy()})).unwrap();
    assert_eq!(r["removed"], 1);
    assert!(moved.is_file(), "removing from the list leaves the file");
    let v = list(&mut e.s, json!({}));
    assert_eq!(names(&v, "other"), ["Far"]);
}

#[test]
fn import_copies_projects_and_refuses_garbage() {
    let mut e = env("import");
    let src = e.dir.join("In/Shoot.fcproj");
    save_project(&mut e.s, &src, 1920, 1080, 25.0);
    let r = e.s.execute("projects.import", json!({"path": src.to_string_lossy()})).unwrap();
    assert_eq!(r["path"], json!(e.root.join("Shoot.fcproj").to_string_lossy()));
    let r = e.s.execute("projects.import", json!({"path": src.to_string_lossy()})).unwrap();
    assert_eq!(r["path"], json!(e.root.join("Shoot 2.fcproj").to_string_lossy()), "never overwrites");
    let junk = e.dir.join("junk.fcproj");
    std::fs::write(&junk, b"not json").unwrap();
    assert!(e.s.execute("projects.import", json!({"path": junk.to_string_lossy()})).is_err());
    // an archive folder comes in whole (a background copy; `wait` for the test)
    std::fs::write(e.dir.join("In/media.bin"), [1u8; 64]).unwrap();
    let r = e.s.execute("projects.import", json!({"path": e.dir.join("In").to_string_lossy(), "wait": true})).unwrap();
    assert!(r["job"].is_u64());
    assert!(e.root.join("In/Shoot.fcproj").is_file() && e.root.join("In/media.bin").is_file());
    // a folder without a project, or the projects folder itself, is refused
    std::fs::create_dir_all(e.dir.join("Empty")).unwrap();
    assert!(e.s.execute("projects.import", json!({"path": e.dir.join("Empty").to_string_lossy()})).is_err());
    assert!(e.s.execute("projects.import", json!({"path": e.root.to_string_lossy()})).is_err());
}

#[test]
fn export_without_sequences_writes_the_project_into_the_folder() {
    let mut e = env("export");
    e.s.execute("projects.new", json!({"name": "Bare"})).unwrap();
    let dest = e.dir.join("Archive");
    let r = e.s.execute("projects.export", json!({"path": e.root.join("Bare.fcproj").to_string_lossy(), "destination": dest.to_string_lossy()})).unwrap();
    assert!(dest.join("Bare.fcproj").is_file(), "{r}");
    assert!(
        e.s.execute("projects.export", json!({"path": e.root.join("Bare.fcproj").to_string_lossy(), "destination": dest.to_string_lossy()})).is_err(),
        "never overwrites"
    );
    // with a sequence: the collect engine (File ▸ Project Manager) writes the archive
    let p = e.root.join("Seq.fcproj");
    save_project(&mut e.s, &p, 640, 360, 24.0);
    let dest2 = e.dir.join("Archive2");
    e.s.execute("projects.export", json!({"path": p.to_string_lossy(), "destination": dest2.to_string_lossy(), "wait": true})).unwrap();
    assert!(dest2.join("Seq.fcproj").is_file());
}

#[test]
fn startup_follows_the_flag_then_the_setting_then_the_workflow() {
    let mut p = crate::autosave::Preferences { workflow: Workflow::Resolve, ..Default::default() };
    assert_eq!(projects::startup_action(&p, false, true), StartupAction::ProjectManager, "Resolve: the home screen");
    assert_eq!(projects::startup_action(&p, true, true), StartupAction::Demo, "--demo wins");
    assert_eq!(projects::startup_action(&p, true, false), StartupAction::NewEmpty, "--empty wins");
    p.workflow = Workflow::Premiere;
    assert_eq!(projects::startup_action(&p, false, true), StartupAction::Demo, "Premiere: today's Show Home");
    p.general.at_startup = "openMostRecent".into();
    assert_eq!(projects::startup_action(&p, false, true), StartupAction::LastProject);
    p.general.at_startup = "emptyProject".into();
    assert_eq!(projects::startup_action(&p, false, true), StartupAction::NewEmpty);
    p.general.startup_show = Some(StartupShow::ProjectManager);
    assert_eq!(projects::startup_action(&p, false, true), StartupAction::ProjectManager, "chosen: either workflow");
    p.workflow = Workflow::Resolve;
    p.general.startup_show = Some(StartupShow::LastProject);
    assert_eq!(projects::startup_action(&p, false, true), StartupAction::LastProject);
    p.general.startup_show = Some(StartupShow::NewEmpty);
    assert_eq!(projects::startup_action(&p, false, true), StartupAction::NewEmpty);
}

#[test]
fn startup_and_folder_prefs_load_old_garbage_and_round_trip() {
    let dir = tmp_dir("rs2-prefs");
    let path = dir.join("preferences.json");
    // a v3 file from before RS2
    std::fs::write(&path, r#"{"version":3,"general":{"atStartup":"openMostRecent"}}"#).unwrap();
    let p = crate::autosave::Preferences::load(&path);
    assert_eq!(p.general.at_startup, "openMostRecent");
    assert_eq!((p.general.startup_show, p.general.projects_folder.clone()), (None, None));
    // garbage values fall back without losing the rest of the file
    std::fs::write(&path, r#"{"version":3,"general":{"atStartup":"openMostRecent","startupShow":42,"projectsFolder":["x"]}}"#).unwrap();
    let p = crate::autosave::Preferences::load(&path);
    assert_eq!(p.general.at_startup, "openMostRecent");
    assert_eq!((p.general.startup_show, p.general.projects_folder.clone()), (None, None));
    // through prefs.set and back
    let mut s = Session { prefs_path: Some(path.clone()), ..Default::default() };
    s.execute("prefs.set", json!({"key": "general.startupShow", "value": "lastProject"})).unwrap();
    s.execute("prefs.set", json!({"key": "general.projectsFolder", "value": "/tmp/fc projects"})).unwrap();
    assert!(s.execute("prefs.set", json!({"key": "general.startupShow", "value": "sometimes"})).is_err());
    let p = crate::autosave::Preferences::load(&path);
    assert_eq!(p.general.startup_show, Some(StartupShow::LastProject));
    assert_eq!(p.general.projects_folder, Some(PathBuf::from("/tmp/fc projects")));
    s.execute("prefs.set", json!({"key": "general.startupShow", "value": "workflowDefault"})).unwrap();
    s.execute("prefs.set", json!({"key": "general.projectsFolder", "value": ""})).unwrap();
    let p = crate::autosave::Preferences::load(&path);
    assert_eq!((p.general.startup_show, p.general.projects_folder), (None, None));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn projects_show_sits_after_open_and_resolve_maps_shift_1() {
    let ids: Vec<&str> = crate::commands::command_specs().iter().map(|c| c.id).collect();
    let pos = |id: &str| ids.iter().position(|x| *x == id).unwrap();
    assert_eq!(pos("projects.show"), pos("file.open") + 1);
    let sc = crate::shortcuts::Shortcuts::new();
    let resolve = sc.builtin(crate::shortcuts::RESOLVE_PRESET).unwrap();
    assert!(resolve.iter().any(|b| b.command == "projects.show" && b.keys == "Shift+1"));
    assert!(!resolve.iter().any(|b| b.command != "projects.show" && b.keys == "Shift+1" && b.panel.is_none()), "Shift+1 is the Project Manager only");
}

#[test]
fn listing_without_the_folder_yet_is_empty_not_an_error() {
    let mut e = env("absent");
    let v = list(&mut e.s, json!({}));
    assert_eq!(v["exists"], false);
    assert!(!e.root.exists(), "listing doesn't create the folder");
    assert_eq!(SortKey::parse("modified"), Some(SortKey::Modified));
}

// ------------------------------------------------------------------ review fixes (RS2)

#[test]
fn a_broad_or_relative_projects_folder_is_refused() {
    let home = PathBuf::from("/home/someone");
    let homes = [home.clone()];
    assert!(projects::unsafe_root(Path::new("relative/Projects"), &homes).is_some(), "relative");
    assert!(projects::unsafe_root(Path::new("/"), &homes).is_some(), "the file system root");
    assert!(projects::unsafe_root(&home, &homes).is_some(), "home itself");
    assert!(projects::unsafe_root(Path::new("/home"), &homes).is_some(), "a parent of home");
    assert!(projects::unsafe_root(Path::new("/home/someone/../someone/Videos"), &homes).is_some(), "`..`");
    assert!(projects::unsafe_root(&home.join("Videos/FilmCraft Projects"), &homes).is_none());
    assert!(projects::unsafe_root(Path::new("/srv/projects"), &homes).is_none());
    // a session with an unsafe preference falls back to the default folder and says why
    let mut e = env("broad");
    for bad in [PathBuf::from("Projects"), PathBuf::from("/")] {
        e.s.prefs.general.projects_folder = Some(bad.clone());
        assert_ne!(projects::projects_root(&e.s), Some(bad.clone()));
        assert!(projects::root_warning(&e.s).is_some());
        let v = list(&mut e.s, json!({}));
        assert!(v["warning"].is_string(), "{v}");
        // nothing outside a real projects folder can be renamed through it
        assert!(e.s.execute("projects.rename", json!({"path": e.dir.join("data").to_string_lossy(), "name": "x"})).is_err());
    }
    if let Some(h) = crate::media_browser::std_home_dir() {
        e.s.prefs.general.projects_folder = Some(PathBuf::from(&h));
        assert_ne!(projects::projects_root(&e.s), Some(PathBuf::from(&h)), "the real home folder is refused");
    }
}

#[test]
fn hidden_and_support_folders_are_not_renamed_or_trashed() {
    let mut e = env("support");
    for d in [".hidden", "Auto-Save", "Proxies", "Client/Proxies"] {
        std::fs::create_dir_all(e.root.join(d)).unwrap();
        for cmd in ["projects.rename", "projects.trash"] {
            let r = e.s.execute(cmd, json!({"path": e.root.join(d).to_string_lossy(), "name": "x"}));
            assert!(r.is_err(), "{cmd} {d}");
        }
        assert!(e.root.join(d).is_dir());
    }
    save_project(&mut e.s, &e.root.join("Auto-Save/Auto.fcproj"), 64, 36, 24.0);
    e.s.execute("file.newProject", json!({"name": "other"})).unwrap();
    assert!(e.s.execute("projects.trash", json!({"path": e.root.join("Auto-Save/Auto.fcproj").to_string_lossy()})).is_err());
}

#[cfg(unix)]
#[test]
fn links_inside_the_folder_are_acted_on_never_their_targets() {
    let mut e = env("links");
    let outside = e.dir.join("Outside");
    save_project(&mut e.s, &outside.join("Far.fcproj"), 64, 36, 24.0);
    e.s.execute("file.newProject", json!({"name": "other"})).unwrap();
    std::fs::create_dir_all(&e.root).unwrap();
    std::os::unix::fs::symlink(&outside, e.root.join("LinkDir")).unwrap();
    std::os::unix::fs::symlink(outside.join("Far.fcproj"), e.root.join("Linked.fcproj")).unwrap();
    // rename: the link is renamed, the target keeps its name
    e.s.execute("projects.rename", json!({"path": e.root.join("LinkDir").to_string_lossy(), "name": "Renamed"})).unwrap();
    assert!(std::fs::symlink_metadata(e.root.join("Renamed")).unwrap().file_type().is_symlink());
    assert!(outside.is_dir() && outside.join("Far.fcproj").is_file());
    e.s.execute("projects.rename", json!({"path": e.root.join("Linked.fcproj").to_string_lossy(), "name": "Other"})).unwrap();
    assert!(std::fs::symlink_metadata(e.root.join("Other.fcproj")).unwrap().file_type().is_symlink());
    assert!(outside.join("Far.fcproj").is_file());
    // trash: the links go, the targets stay
    e.s.execute("projects.trash", json!({"path": e.root.join("Renamed").to_string_lossy()})).unwrap();
    e.s.execute("projects.trash", json!({"path": e.root.join("Other.fcproj").to_string_lossy()})).unwrap();
    assert!(outside.join("Far.fcproj").is_file(), "the target outside is untouched");
    assert!(std::fs::symlink_metadata(e.dir.join("Trash/files/Renamed")).unwrap().file_type().is_symlink());
    // a link pointing outside, reached by a path through it, is not a folder inside the root
    std::os::unix::fs::symlink(&outside, e.root.join("Again")).unwrap();
    std::fs::create_dir_all(outside.join("Sub")).unwrap();
    assert!(e.s.execute("projects.trash", json!({"path": e.root.join("Again/Sub").to_string_lossy()})).is_err());
    assert!(outside.join("Sub").is_dir());
}

#[cfg(unix)]
#[test]
fn a_dangling_link_counts_as_taken() {
    let mut e = env("dangling");
    save_project(&mut e.s, &e.root.join("One.fcproj"), 64, 36, 24.0);
    e.s.execute("file.newProject", json!({"name": "other"})).unwrap();
    let decoy = e.dir.join("decoy-target");
    std::os::unix::fs::symlink(&decoy, e.root.join("Two.fcproj")).unwrap();
    std::os::unix::fs::symlink(&decoy, e.root.join("One copy.fcproj")).unwrap();
    let one = e.root.join("One.fcproj");
    assert!(e.s.execute("projects.rename", json!({"path": one.to_string_lossy(), "name": "Two"})).is_err());
    assert!(e.s.execute("projects.duplicate", json!({"path": one.to_string_lossy(), "name": "Two"})).is_err());
    let d = e.s.execute("projects.duplicate", json!({"path": one.to_string_lossy()})).unwrap();
    assert_eq!(d["name"], "One copy 2");
    assert!(!decoy.exists(), "nothing was written through the link");
    // write_new never replaces
    assert_eq!(projects::write_new(&one, b"x").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    assert!(projects::move_no_replace(&e.root.join("One copy 2.fcproj"), &one).is_err());
    assert!(e.root.join("One copy 2.fcproj").is_file());
}

#[test]
fn folder_renames_and_trash_carry_recents_and_cache() {
    let mut e = env("prefix");
    let a = e.root.join("Client/A.fcproj");
    save_project(&mut e.s, &a, 64, 36, 24.0);
    e.s.execute("file.newProject", json!({"name": "other"})).unwrap();
    let cache = projects::cache_dir(&e.s).unwrap();
    let old_key = projects::cache_key(&a);
    assert!(cache.join(format!("{old_key}.json")).is_file());
    std::fs::write(cache.join(format!("{old_key}.png")), b"png").unwrap();
    // onto an existing folder: refused
    std::fs::create_dir_all(e.root.join("Taken")).unwrap();
    assert!(e.s.execute("projects.rename", json!({"path": e.root.join("Client").to_string_lossy(), "name": "Taken"})).is_err());
    e.s.execute("projects.rename", json!({"path": e.root.join("Client").to_string_lossy(), "name": "Customer"})).unwrap();
    let moved = e.root.join("Customer/A.fcproj");
    assert!(e.s.prefs.general.recent_projects.iter().any(|p| Path::new(p) == moved), "{:?}", e.s.prefs.general.recent_projects);
    assert!(!e.s.prefs.general.recent_projects.iter().any(|p| Path::new(p).starts_with(e.root.join("Client"))));
    let new_key = projects::cache_key(&moved);
    assert!(cache.join(format!("{new_key}.json")).is_file() && cache.join(format!("{new_key}.png")).is_file());
    assert!(!cache.join(format!("{old_key}.json")).exists() && !cache.join(format!("{old_key}.png")).exists());
    // trash: recents and cache entries under it go
    e.s.execute("projects.trash", json!({"path": e.root.join("Customer").to_string_lossy()})).unwrap();
    assert!(!e.s.prefs.general.recent_projects.iter().any(|p| Path::new(p).starts_with(e.root.join("Customer"))));
    assert!(!cache.join(format!("{new_key}.json")).exists() && !cache.join(format!("{new_key}.png")).exists());
}

#[test]
fn a_folder_holding_the_open_project_is_not_trashed_or_renamed() {
    let mut e = env("openfolder");
    let p = e.root.join("Work/Open.fcproj");
    save_project(&mut e.s, &p, 64, 36, 24.0);
    assert!(e.s.execute("projects.trash", json!({"path": e.root.join("Work").to_string_lossy()})).is_err());
    assert!(e.s.execute("projects.rename", json!({"path": e.root.join("Work").to_string_lossy(), "name": "W"})).is_err());
    assert!(p.is_file());
}

#[test]
fn importing_the_projects_folder_or_a_folder_inside_it_is_refused() {
    let mut e = env("importself");
    save_project(&mut e.s, &e.root.join("Sub/A.fcproj"), 64, 36, 24.0);
    for path in [e.root.clone(), e.root.join("Sub"), e.dir.clone()] {
        assert!(e.s.execute("projects.import", json!({"path": path.to_string_lossy(), "wait": true})).is_err(), "{}", path.display());
    }
    // a hostile file never panics the import
    let junk = e.dir.join("junk.fcproj");
    std::fs::write(&junk, [0xffu8, b'{', 0, 0x80]).unwrap();
    assert!(e.s.execute("projects.import", json!({"path": junk.to_string_lossy()})).is_err());
    // removing something that isn't listed removes nothing
    assert_eq!(e.s.execute("projects.removeFromList", json!({"path": "/nowhere.fcproj"})).unwrap()["removed"], 0);
}

#[cfg(unix)]
#[test]
fn a_planted_top_level_trash_folder_is_refused() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let e = env("topdir");
    let uid = std::fs::metadata(&e.dir).unwrap().uid();
    // missing: made with mode 0700
    let t = e.dir.join(".Trash-new");
    projects::checked_trash_dir(&t, uid).unwrap();
    assert_eq!(std::fs::metadata(&t).unwrap().mode() & 0o7777, 0o700);
    // a planted link to somewhere else
    let elsewhere = e.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, e.dir.join(".Trash-link")).unwrap();
    assert!(projects::checked_trash_dir(&e.dir.join(".Trash-link"), uid).is_err());
    // a world-writable folder, or someone else's
    let open = e.dir.join(".Trash-open");
    std::fs::create_dir_all(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(projects::checked_trash_dir(&open, uid).is_err());
    assert!(projects::checked_trash_dir(&t, uid.wrapping_add(1)).is_err());
    // a file
    std::fs::write(e.dir.join(".Trash-file"), b"x").unwrap();
    assert!(projects::checked_trash_dir(&e.dir.join(".Trash-file"), uid).is_err());
}

/// STORAGE-PLAN S1 item 5: a package holds the project, its media and a manifest that checks
/// out; a damaged file is found; an existing package folder is never overwritten.
#[test]
fn a_package_holds_the_project_its_media_and_a_manifest_that_verifies() {
    use crate::media_test_util::{make_movie, session_with};
    use crate::package::{MANIFEST_FILE, Manifest, Role, verify};
    let dir = tmp_dir("s1-package");
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::Forest, 64, 36, 6);
    let (mut s, _, _) = session_with(&[&movie]);
    let proj = dir.join("Edit.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let dest = dir.join("Out");
    // the size estimate first: nothing is written
    let est = s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": dest.to_string_lossy(), "dryRun": true})).unwrap();
    assert!(est["estimateBytes"].as_u64().is_some_and(|b| b >= std::fs::metadata(&movie).unwrap().len()), "{est}");
    assert!(!dest.join("Edit package").exists());
    let r = s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": dest.to_string_lossy(), "wait": true})).unwrap();
    let pkg = dest.join("Edit package");
    assert_eq!(r["package"], json!(pkg.to_string_lossy()), "{r}");
    let m = Manifest::parse(&std::fs::read(pkg.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(m.project, "Edit.fcproj");
    assert!(m.entries.iter().any(|e| e.role == Role::Media && e.path.ends_with("clip.mov")), "{m:?}");
    assert!(m.left_out.iter().any(|l| l.what == "render cache"));
    assert!(!String::from_utf8_lossy(&m.to_json()).contains(&*dir.to_string_lossy()), "no sender paths in the manifest");
    let rep = verify(&pkg, &m, &|| false).unwrap();
    assert!(rep.is_complete() && rep.extra.is_empty(), "{rep:?}");
    // a damaged copy is found
    let clip = pkg.join(&m.entries.iter().find(|e| e.role == Role::Media).unwrap().path);
    let mut bytes = std::fs::read(&clip).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&clip, bytes).unwrap();
    assert_eq!(verify(&pkg, &m, &|| false).unwrap().damaged.len(), 1);
    // never over an existing package
    assert!(s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": dest.to_string_lossy()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A package whose copy fails leaves nothing behind.
#[test]
fn a_failed_package_is_removed() {
    use crate::media_test_util::{make_movie, session_with};
    let dir = tmp_dir("s1-package-fail");
    let movie = dir.join("clip.mov");
    make_movie(&movie, filmcraft_media::DemoScene::Forest, 64, 36, 3);
    let (mut s, _, _) = session_with(&[&movie]);
    let proj = dir.join("Gone.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    std::fs::remove_file(&movie).unwrap();
    let dest = dir.join("Out");
    let _ = s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": dest.to_string_lossy(), "wait": true}));
    assert!(!dest.join("Gone package").exists(), "partial package removed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// STORAGE-PLAN S1 item 5c: a package comes in checked and relinked to its copies (the
/// originals can be gone); a damaged package is refused and leaves nothing behind.
#[test]
fn an_imported_package_is_checked_and_relinked_by_itself() {
    use crate::media_test_util::{make_movie, session_with};
    let dir = tmp_dir("s1-import-package");
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::Dunes, 64, 36, 4);
    let (mut s, _, _) = session_with(&[&movie]);
    let root = dir.join("Projects");
    s.prefs.general.projects_folder = Some(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    let proj = dir.join("Trip.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let out = dir.join("Out");
    s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": out.to_string_lossy(), "wait": true})).unwrap();
    let pkg = out.join("Trip package");
    // the sender's files are gone: only the package is left
    std::fs::remove_dir_all(dir.join("Shoot")).unwrap();
    let r = s.execute("projects.importPackage", json!({"path": pkg.to_string_lossy(), "wait": true})).unwrap();
    assert_eq!(r["package"], true, "{r}");
    let to = PathBuf::from(r["path"].as_str().unwrap());
    let mut t = Session::default();
    t.execute("file.open", json!({"path": to.join("Trip.fcproj").to_string_lossy()})).unwrap();
    let paths: Vec<String> = t
        .project
        .items
        .values()
        .filter_map(|i| match &i.as_media()?.media {
            filmcraft_project::MediaRef::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|p| p.starts_with(&*to.to_string_lossy()) && Path::new(p).is_file()), "{paths:?}");
    assert!(t.offline.missing.is_empty(), "nothing to relink");
    // a damaged package: refused, nothing left behind
    let clip = std::fs::read_dir(&pkg).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "mov")).unwrap();
    std::fs::write(&clip, b"not the clip").unwrap();
    let before: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.path()).collect();
    let _ = s.execute("projects.importPackage", json!({"path": pkg.to_string_lossy(), "wait": true}));
    let job = s.jobs.last().unwrap();
    let err = job.progress.error.lock().unwrap().clone().unwrap_or_default();
    assert!(err.contains("incomplete"), "{err}");
    let after: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(before.len(), after.len(), "nothing left behind");
    // a plain folder is not a package
    std::fs::remove_file(pkg.join("manifest.json")).unwrap();
    assert!(s.execute("projects.importPackage", json!({"path": pkg.to_string_lossy()})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// S1 item 5d: a package carries the gallery stills' pictures (PNGs outside the project file),
/// and its project takes the receiver's scratch disks, so the stills sit inside the package and
/// are found after import even when the sender kept them on a scratch disk.
#[test]
fn a_package_carries_the_gallery_stills() {
    use crate::media_test_util::{make_movie, session_with};
    let dir = tmp_dir("s1-package-stills");
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::Forest, 64, 36, 4);
    let (mut s, _, _) = session_with(&[&movie]);
    let root = dir.join("Projects");
    s.prefs.general.projects_folder = Some(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    let proj = dir.join("Look.fcproj");
    {
        let p = std::sync::Arc::make_mut(&mut s.project);
        p.settings.scratch.video_previews = Some(dir.join("Scratch").to_string_lossy().into_owned());
        p.stills.push(filmcraft_project::Still {
            id: 1,
            label: "warm".into(),
            clip: None,
            clip_name: String::new(),
            sequence: None,
            time: filmcraft_time::Tick::ZERO,
            grade: None,
            width: 2,
            height: 2,
            nonce: 1,
        });
    }
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let stills = crate::project_tools::stills_dir_for(&s.project, &proj.to_string_lossy());
    assert!(stills.starts_with(dir.join("Scratch")));
    std::fs::create_dir_all(&stills).unwrap();
    std::fs::write(stills.join("still-1.png"), b"png bytes").unwrap();
    let out = dir.join("Out");
    let est = s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": out.to_string_lossy(), "dryRun": true})).unwrap();
    s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": out.to_string_lossy(), "wait": true})).unwrap();
    let pkg = out.join("Look package");
    let m = crate::package::Manifest::parse(&std::fs::read(pkg.join(crate::package::MANIFEST_FILE)).unwrap()).unwrap();
    assert!(m.entries.iter().any(|e| e.path.ends_with("still-1.png")), "{m:?}");
    assert!(est["estimateBytes"].as_u64().is_some(), "{est}");
    std::fs::remove_dir_all(dir.join("Scratch")).unwrap();
    std::fs::remove_dir_all(dir.join("Shoot")).unwrap();
    let r = s.execute("projects.importPackage", json!({"path": pkg.to_string_lossy(), "wait": true})).unwrap();
    let to = PathBuf::from(r["path"].as_str().unwrap()).join("Look.fcproj");
    let mut t = Session::default();
    t.execute("file.open", json!({"path": to.to_string_lossy()})).unwrap();
    assert_eq!(t.project.settings.scratch.video_previews, None, "the sender's scratch disks don't travel");
    assert_eq!(t.project.stills.len(), 1);
    let got = crate::project_tools::stills_dir_for(&t.project, &to.to_string_lossy()).join("still-1.png");
    assert_eq!(std::fs::read(&got).unwrap(), b"png bytes", "{}", got.display());
    let _ = std::fs::remove_dir_all(&dir);
}

/// S1 item 5d: a package carries the user shader effects its project uses (and only those), and
/// importing it installs them into this computer's Effects folder without overwriting one there.
#[test]
fn a_package_carries_the_user_effects_it_uses() {
    use crate::media_test_util::{make_movie, session_with};
    const GLOW: &str = "// @effect \"Pack Glow\"\n// @param amount: float = 0.5 [0.0, 1.0] \"Amount\"\nfn effect(px: vec2<f32>) -> vec4<f32> {\n    let c = sample(px);\n    return vec4(mix(c.rgb, vec3(1.0), P.amount * 0.2), c.a);\n}\n";
    let dir = tmp_dir("s1-package-effects");
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::Forest, 64, 36, 4);
    let (mut s, _, _) = session_with(&[&movie]);
    let sender = dir.join("sender-data");
    std::fs::create_dir_all(sender.join("Effects")).unwrap();
    std::fs::write(sender.join("Effects").join("pack-glow.wgsl"), GLOW).unwrap();
    std::fs::write(sender.join("Effects").join("unused.wgsl"), GLOW.replace("Pack Glow", "Unused")).unwrap();
    s.prefs_path = Some(sender.join("preferences.json"));
    crate::user_effects::reload(&mut s);
    let root = dir.join("Projects");
    s.prefs.general.projects_folder = Some(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    s.execute("effects.apply", json!({"clips": [clip.0], "effect": "user.pack-glow"})).unwrap();
    let proj = dir.join("Fx.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let out = dir.join("Out");
    s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": out.to_string_lossy(), "wait": true})).unwrap();
    let pkg = out.join("Fx package");
    let m = crate::package::Manifest::parse(&std::fs::read(pkg.join(crate::package::MANIFEST_FILE)).unwrap()).unwrap();
    let shaders: Vec<&str> = m.entries.iter().filter(|e| e.role == crate::package::Role::Shader).map(|e| e.path.as_str()).collect();
    assert_eq!(shaders, ["Effects/pack-glow.wgsl"], "only the effect the project uses");
    // the receiver: no such effect yet, and an unrelated one of its own that stays as it is
    let receiver = dir.join("receiver-data");
    std::fs::create_dir_all(receiver.join("Effects")).unwrap();
    s.prefs_path = Some(receiver.join("preferences.json"));
    let r = s.execute("projects.importPackage", json!({"path": pkg.to_string_lossy(), "wait": true})).unwrap();
    assert_eq!(r["package"], true, "{r}");
    assert_eq!(std::fs::read_to_string(receiver.join("Effects").join("pack-glow.wgsl")).unwrap(), GLOW);
    // a second import never overwrites the receiver's own (changed) file
    std::fs::write(receiver.join("Effects").join("pack-glow.wgsl"), "// mine").unwrap();
    s.execute("projects.importPackage", json!({"path": pkg.to_string_lossy(), "wait": true})).unwrap();
    assert_eq!(std::fs::read_to_string(receiver.join("Effects").join("pack-glow.wgsl")).unwrap(), "// mine");
    let _ = std::fs::remove_dir_all(&dir);
}

/// STORAGE-PLAN S1 item 5: a package whose project was saved by a newer FilmCraft still imports
/// (checked and complete) instead of failing; opening it then says to update FilmCraft.
#[test]
fn a_package_from_a_newer_filmcraft_imports_and_opening_says_update() {
    use crate::media_test_util::{make_movie, session_with};
    use crate::package::{MANIFEST_FILE, Manifest, Role, sha256_file};
    let dir = tmp_dir("s1-package-newer");
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::Dunes, 64, 36, 4);
    let (mut s, _, _) = session_with(&[&movie]);
    let root = dir.join("Projects");
    s.prefs.general.projects_folder = Some(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    let proj = dir.join("Future.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let out = dir.join("Out");
    s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": out.to_string_lossy(), "wait": true})).unwrap();
    let pkg = out.join("Future package");
    // as if a newer FilmCraft had saved it: a higher schema, and a manifest that matches it
    let fc = pkg.join("Future.fcproj");
    let text = std::fs::read_to_string(&fc).unwrap();
    let key = format!("\"schema_version\":{}", filmcraft_format::SCHEMA_VERSION);
    assert!(text.contains(&key), "{}", &text[..120.min(text.len())]);
    std::fs::write(&fc, text.replacen(&key, "\"schema_version\":9999", 1)).unwrap();
    let mut m = Manifest::parse(&std::fs::read(pkg.join(MANIFEST_FILE)).unwrap()).unwrap();
    let (sha, size) = sha256_file(&fc).unwrap();
    for e in m.entries.iter_mut().filter(|e| e.role == Role::Project) {
        (e.sha256, e.size) = (sha.clone(), size);
    }
    std::fs::write(pkg.join(MANIFEST_FILE), m.to_json()).unwrap();
    let r = s.execute("projects.importPackage", json!({"path": pkg.to_string_lossy(), "wait": true})).unwrap();
    let job = s.jobs.last().unwrap();
    assert_eq!(*job.progress.error.lock().unwrap(), None, "imported, not failed");
    let to = PathBuf::from(r["path"].as_str().unwrap()).join("Future.fcproj");
    assert!(to.is_file());
    let err = Session::default().execute("file.open", json!({"path": to.to_string_lossy()})).unwrap_err().to_string();
    assert!(err.contains("newer version of Revolution"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Relinking an unpacked package only takes paths inside the sender's package folder: a clip
/// elsewhere on the sender's disk that merely has the same file name is left alone.
#[test]
fn package_relink_ignores_paths_that_only_end_like_an_entry() {
    use crate::media_test_util::{make_movie, session_with};
    use crate::package::{Entry, Manifest, Role};
    let dir = tmp_dir("s1-relink-tail");
    let (pkg, elsewhere, new) = (dir.join("sender/pkg"), dir.join("elsewhere"), dir.join("new"));
    for d in [&pkg, &elsewhere, &new] {
        std::fs::create_dir_all(d).unwrap();
    }
    for f in [pkg.join("a.mov"), pkg.join("b.mov"), elsewhere.join("a.mov")] {
        make_movie(&f, filmcraft_media::DemoScene::Plasma, 32, 18, 2);
    }
    let (mut s, _, _) = session_with(&[&pkg.join("a.mov"), &pkg.join("b.mov"), &elsewhere.join("a.mov")]);
    s.execute("file.saveAs", json!({"path": new.join("T.fcproj").to_string_lossy()})).unwrap();
    let mut m = Manifest::new("T.fcproj");
    for (path, role) in [("T.fcproj", Role::Project), ("a.mov", Role::Media), ("b.mov", Role::Media)] {
        m.entries.push(Entry { path: path.into(), size: 1, sha256: "0".repeat(64), role });
    }
    assert_eq!(projects::relink_package(&new, &m).unwrap(), 2);
    let mut t = Session::default();
    t.execute("file.open", json!({"path": new.join("T.fcproj").to_string_lossy()})).unwrap();
    let mut paths: Vec<String> = t
        .project
        .items
        .values()
        .filter_map(|i| match &i.as_media()?.media {
            filmcraft_project::MediaRef::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    paths.sort();
    let mut want = vec![
        elsewhere.join("a.mov").to_string_lossy().into_owned(),
        new.join("a.mov").to_string_lossy().into_owned(),
        new.join("b.mov").to_string_lossy().into_owned(),
    ];
    want.sort();
    assert_eq!(paths, want);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A one-file `.revpkg` (an older `.fcpackage` name still imports): written, the folder it came from removed, and imported back with
/// every clip relinked to the unpacked copies.
#[test]
fn a_one_file_package_round_trips() {
    use crate::media_test_util::{make_movie, session_with};
    let dir = tmp_dir("s1-fcpackage");
    let movie = dir.join("Shoot").join("clip.mov");
    std::fs::create_dir_all(movie.parent().unwrap()).unwrap();
    make_movie(&movie, filmcraft_media::DemoScene::OceanSunset, 64, 36, 4);
    let (mut s, _, _) = session_with(&[&movie]);
    let root = dir.join("Projects");
    s.prefs.general.projects_folder = Some(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    let proj = dir.join("Reel.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    let out = dir.join("Out");
    let r =
        s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": out.to_string_lossy(), "format": "file", "wait": true})).unwrap();
    let file = out.join("Reel.revpkg");
    assert_eq!(r["package"], json!(file.to_string_lossy()), "{r}");
    assert!(file.is_file() && !out.join("Reel package").exists(), "one file, no folder left");
    assert!(
        s.execute("projects.package", json!({"path": proj.to_string_lossy(), "destination": out.to_string_lossy(), "format": "file"})).is_err(),
        "never over an existing package"
    );
    std::fs::remove_dir_all(dir.join("Shoot")).unwrap();
    // a package made before 0.1.3 had the old extension: it still opens
    let legacy = out.join("Reel.fcpackage");
    std::fs::rename(&file, &legacy).unwrap();
    let file = legacy;
    let r = s.execute("projects.importPackage", json!({"path": file.to_string_lossy(), "wait": true})).unwrap();
    let to = PathBuf::from(r["path"].as_str().unwrap());
    assert!(s.jobs.last().unwrap().progress.error.lock().unwrap().is_none());
    let mut t = Session::default();
    t.execute("file.open", json!({"path": to.join("Reel.fcproj").to_string_lossy()})).unwrap();
    let ok = t.project.items.values().filter_map(|i| i.as_media()).all(|m| match &m.media {
        filmcraft_project::MediaRef::File { path } => path.starts_with(&*to.to_string_lossy()) && Path::new(path).is_file(),
        _ => true,
    });
    assert!(ok);
    // a damaged package file: refused, nothing left
    let mut bytes = std::fs::read(&file).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&file, bytes).unwrap();
    let before = std::fs::read_dir(&root).unwrap().count();
    let _ = s.execute("projects.importPackage", json!({"path": file.to_string_lossy(), "wait": true}));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), before, "nothing left behind");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fork §1a.108: a project saved without a poster gets one made from its timeline (a frame with
/// something on it), and projects that can't give one fail cleanly.
#[test]
fn a_project_without_a_poster_gets_one_from_its_timeline() {
    let e = env("backfill");
    let movie = e.dir.join("clip.mp4");
    crate::media_test_util::make_movie(&movie, filmcraft_media::DemoScene::Aurora, 96, 54, 12);
    let (mut s, _, _) = crate::media_test_util::session_with(&[&movie]);
    let project = e.root.join("Clip.fcproj");
    std::fs::create_dir_all(&e.root).unwrap();
    s.execute("file.saveAs", json!({"path": project.to_string_lossy()})).unwrap();
    let cache = e.dir.join("cache");
    let services = Session::default().services.clone();
    projects::make_poster(services.clone(), &cache, &project).unwrap();
    let s = projects::summary(Some(&cache), &project);
    let png = s.poster.expect("a poster is stored");
    let (w, h, px) = projects::load_poster(Path::new(&png)).unwrap();
    assert_eq!((w, h), (96, 54));
    assert!(px.as_chunks::<4>().0.iter().any(|p| p[..3].iter().any(|c| *c > 16)), "not a black frame");

    // an empty timeline, garbage and a missing file: an error, no poster, no panic
    let mut empty = Session::default();
    let empty_path = e.root.join("Empty.fcproj");
    save_project(&mut empty, &empty_path, 640, 360, 24.0);
    let garbage = e.root.join("Garbage.fcproj");
    std::fs::write(&garbage, b"\x00not a project").unwrap();
    for p in [empty_path, garbage, e.root.join("Missing.fcproj")] {
        assert!(projects::make_poster(services.clone(), &cache, &p).is_err(), "{}", p.display());
        assert!(projects::summary(Some(&cache), &p).poster.is_none());
    }
}
