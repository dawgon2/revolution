//! Grade copy / paste / append, stills and the still comparison (GRADE-PLAN G1.4–G1.5).

use serde_json::{Value, json};

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_project::grade::{Grade, MAX_NODES};

/// A demo session with two video clips: (session, first, second).
fn demo() -> (Session, ClipId, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let a = q.video_tracks[0].item_at(s.playhead()).unwrap().id;
    let b = q.video_tracks[0].items.iter().map(|i| i.id).find(|i| *i != a).unwrap();
    s.state.selection = vec![a];
    (s, a, b)
}

fn grade(s: &Session, c: ClipId) -> Option<Grade> {
    s.active_sequence().unwrap().find_item(c).unwrap().1.grade.clone()
}

fn add_node(s: &mut Session, c: ClipId, sat: f64) -> u64 {
    let n = s.execute("grade.addNode", json!({"clip": c.0})).unwrap()["node"].as_u64().unwrap();
    s.execute("grade.setParam", json!({"clip": c.0, "node": n, "param": "saturation", "value": sat})).unwrap();
    n
}

#[test]
fn copy_paste_replaces_in_one_undo_step() {
    let (mut s, a, b) = demo();
    add_node(&mut s, a, 1.5);
    add_node(&mut s, b, 0.5);
    add_node(&mut s, b, 0.6);
    let r = s.execute("grade.copy", json!({})).unwrap();
    assert_eq!(r["nodes"], json!(1));
    let undo = s.history.undo.len();
    let r = s.execute("grade.paste", json!({"clips": [b.0]})).unwrap();
    assert_eq!(r["clips"], json!(1));
    assert_eq!(grade(&s, b), grade(&s, a));
    assert_eq!(s.history.undo.len(), undo + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(grade(&s, b).unwrap().nodes.len(), 2);
}

#[test]
fn paste_defaults_to_selected_video_clips_and_needs_a_copy() {
    let (mut s, a, b) = demo();
    add_node(&mut s, a, 1.5);
    assert!(s.execute("grade.paste", json!({"clips": [b.0]})).is_err(), "nothing copied yet");
    assert!(s.execute("grade.copy", json!({"clip": b.0})).is_err(), "an ungraded clip has nothing to copy");
    s.execute("grade.copy", json!({})).unwrap();
    s.state.selection = vec![b];
    s.execute("grade.paste", json!({})).unwrap();
    assert_eq!(grade(&s, b), grade(&s, a));
    s.state.selection.clear();
    assert!(s.execute("grade.paste", json!({})).is_err());
}

#[test]
fn append_adds_nodes_with_fresh_ids_and_respects_the_cap() {
    let (mut s, a, b) = demo();
    let n1 = add_node(&mut s, a, 1.5);
    let n2 = add_node(&mut s, a, 1.2);
    add_node(&mut s, b, 0.5);
    s.execute("grade.copy", json!({})).unwrap();
    s.execute("grade.append", json!({"clips": [b.0]})).unwrap();
    let g = grade(&s, b).unwrap();
    assert_eq!(g.nodes.len(), 3);
    let mut ids: Vec<u64> = g.nodes.iter().map(|n| n.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3, "ids stay unique");
    assert_eq!(g.nodes[1].params, grade(&s, a).unwrap().node(n1).unwrap().params);
    assert_eq!(g.nodes[2].params, grade(&s, a).unwrap().node(n2).unwrap().params);
    // appending to a clip with no grade creates one
    let c = s.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| i.id).find(|i| *i != a && *i != b);
    if let Some(c) = c {
        s.execute("grade.append", json!({"clips": [c.0]})).unwrap();
        assert_eq!(grade(&s, c).unwrap().nodes.len(), 2);
    }
    // the cap: fill b, then appending refuses; one short of the cap takes one node
    for _ in 0..(MAX_NODES - 3) {
        s.execute("grade.addNode", json!({"clip": b.0})).unwrap();
    }
    assert_eq!(grade(&s, b).unwrap().nodes.len(), MAX_NODES);
    assert!(s.execute("grade.append", json!({"clips": [b.0]})).is_err());
    s.execute("grade.removeNode", json!({"clip": b.0, "node": g.nodes[0].id})).unwrap();
    s.execute("grade.append", json!({"clips": [b.0]})).unwrap();
    assert_eq!(grade(&s, b).unwrap().nodes.len(), MAX_NODES);
}

#[test]
fn apply_from_copies_one_clips_grade_to_the_others() {
    let (mut s, a, b) = demo();
    add_node(&mut s, a, 1.5);
    s.state.selection = vec![a, b];
    let r = s.execute("grade.applyFrom", json!({"from": a.0})).unwrap();
    assert_eq!(r["clips"], json!(1), "the source clip is left out");
    assert_eq!(grade(&s, b), grade(&s, a));
    assert!(s.execute("grade.applyFrom", json!({"from": b.0, "clips": [987_654_321u64]})).is_err());
    assert!(s.execute("grade.applyFrom", json!({"clips": [b.0]})).is_err(), "needs `from`");
    assert!(s.execute("grade.applyFrom", json!({"from": 987_654_321u64, "clips": [b.0]})).is_err());
    assert!(s.execute("grade.applyFrom", json!({"from": "x"})).is_err());
}

#[test]
fn hostile_targets_are_refused_without_changes() {
    let (mut s, a, _) = demo();
    add_node(&mut s, a, 1.5);
    s.execute("grade.copy", json!({})).unwrap();
    let before = s.project.clone();
    for p in [json!({"clips": [987_654_321u64]}), json!({"clips": ["x"]}), json!({"clips": 5}), json!({"clips": [-1]}), json!({"clips": []})] {
        assert!(s.execute("grade.paste", p.clone()).is_err(), "{p}");
        assert!(s.execute("grade.append", p.clone()).is_err(), "{p}");
    }
    assert!(std::sync::Arc::ptr_eq(&before, &s.project));
}

#[test]
fn paste_attributes_carries_the_grade() {
    let (mut s, a, b) = demo();
    add_node(&mut s, a, 1.5);
    s.state.selection = vec![a];
    s.execute("edit.copy", json!({})).unwrap();
    s.execute("edit.pasteAttributes", json!({"clips": [b.0], "grade": false})).unwrap();
    assert!(grade(&s, b).is_none(), "grade:false leaves the grade out");
    s.execute("edit.pasteAttributes", json!({"clips": [b.0]})).unwrap();
    assert_eq!(grade(&s, b), grade(&s, a));
}

fn grab(s: &mut Session, p: Value) -> u64 {
    s.execute("stills.grab", p).unwrap()["still"].as_u64().unwrap()
}

#[test]
fn grab_stores_metadata_and_picture() {
    let (mut s, a, _) = demo();
    add_node(&mut s, a, 0.0);
    let r = s.execute("stills.grab", json!({"label": "Hero\nshot"})).unwrap();
    let id = r["still"].as_u64().unwrap();
    assert_eq!(r["picture"], json!(true));
    assert!(r["width"].as_u64().unwrap() <= 1920);
    let st = s.project.stills.iter().find(|x| x.id == id).unwrap().clone();
    assert_eq!(st.label, "Heroshot");
    assert_eq!(st.clip, Some(a));
    assert_eq!(st.grade, grade(&s, a));
    let pic = s.still_picture(id).unwrap();
    assert_eq!((pic.w, pic.h), (st.width, st.height));
    assert_eq!(pic.rgba.len(), (pic.w * pic.h * 4) as usize);
    let list = s.execute("stills.list", json!({})).unwrap();
    assert_eq!(list["stills"][0]["id"], json!(id));
    assert_eq!(list["stills"][0]["picture"], json!(true));
    // undo takes the grab back
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.stills.is_empty());
    assert!(s.still_picture(id).is_none());
}

#[test]
fn a_missing_picture_is_rendered_again() {
    let (mut s, _, _) = demo();
    let id = grab(&mut s, json!({}));
    for d in s.stills_dirs() {
        let _ = std::fs::remove_dir_all(d);
    }
    let pic = s.still_picture(id).expect("re-rendered from the sequence time");
    assert!(pic.w > 0 && pic.h > 0);
    s.project = std::sync::Arc::new({
        let mut p = (*s.project).clone();
        p.stills[0].sequence = None;
        p
    });
    assert!(s.still_picture(id).is_none(), "no file and no sequence: a placeholder");
}

#[test]
fn apply_delete_label_and_hostile_ids() {
    let (mut s, a, b) = demo();
    add_node(&mut s, a, 0.2);
    let id = grab(&mut s, json!({"clip": a.0}));
    assert!(s.execute("stills.grab", json!({"clip": "x"})).is_err());
    assert!(s.execute("stills.grab", json!({"clip": 987_654_321u64})).is_err());
    let undo = s.history.undo.len();
    s.execute("stills.apply", json!({"still": id, "clips": [b.0]})).unwrap();
    assert_eq!(grade(&s, b), grade(&s, a));
    assert_eq!(s.history.undo.len(), undo + 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(grade(&s, b).is_none());
    s.execute("stills.label", json!({"still": id, "label": "x".repeat(500)})).unwrap();
    assert_eq!(s.project.stills[0].label.chars().count(), 64);
    let before = s.project.clone();
    for (cmd, p) in [
        ("stills.apply", json!({"still": 999, "clips": [b.0]})),
        ("stills.apply", json!({"still": "x", "clips": [b.0]})),
        ("stills.apply", json!({"clips": [b.0]})),
        ("stills.apply", json!({"still": id, "clips": [987_654_321u64]})),
        ("stills.delete", json!({"still": 999})),
        ("stills.delete", json!({"still": u64::MAX})),
        ("stills.delete", json!({})),
        ("stills.label", json!({"still": 999, "label": "a"})),
        ("stills.label", json!({"still": id})),
    ] {
        assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
    }
    assert!(std::sync::Arc::ptr_eq(&before, &s.project));
    s.execute("stills.delete", json!({"still": id})).unwrap();
    assert!(s.project.stills.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.stills.len(), 1);
    assert!(s.still_picture(id).is_some(), "the picture survives delete + undo");
}

#[test]
fn stills_are_capped() {
    let (mut s, _, _) = demo();
    let mut p = (*s.project).clone();
    for i in 0..filmcraft_project::still::MAX_STILLS {
        p.stills.push(filmcraft_project::Still {
            id: 1_000_000 + i as u64,
            label: String::new(),
            clip: None,
            clip_name: String::new(),
            sequence: None,
            time: filmcraft_time::Tick::ZERO,
            grade: None,
            width: 0,
            height: 0,
            nonce: 0,
        });
    }
    s.project = std::sync::Arc::new(p);
    assert!(s.execute("stills.grab", json!({})).is_err());
}

#[test]
fn compare_state_is_validated_and_follows_deletes() {
    let (mut s, _, _) = demo();
    let c = s.execute("compare.get", json!({})).unwrap();
    assert_eq!(c["mode"], json!("off"));
    assert!(s.execute("compare.set", json!({"mode": "wipe"})).is_err(), "no still chosen");
    let id = grab(&mut s, json!({}));
    for p in [
        json!({"still": 999, "mode": "wipe"}),
        json!({"still": "x"}),
        json!({"still": id, "mode": "bogus"}),
        json!({"still": id, "mode": 3}),
        json!({"still": id, "mode": "wipe", "position": "left"}),
        json!({"still": id, "mode": "wipe", "vertical": 1}),
    ] {
        assert!(s.execute("compare.set", p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.state.compare.mode, super::stills::CompareMode::Off, "refused calls change nothing");
    let r = s.execute("compare.set", json!({"still": id, "mode": "wipe", "position": 7.5, "vertical": true})).unwrap();
    assert_eq!(r["position"], json!(1.0), "clamped");
    assert_eq!(r["active"], json!(true));
    let r = s.execute("compare.set", json!({"mode": "split", "position": 0.25})).unwrap();
    assert_eq!((r["mode"].clone(), r["vertical"].clone()), (json!("split"), json!(true)));
    // the state serialises for the control channel
    assert_eq!(serde_json::to_value(&s.state.compare).unwrap()["mode"], json!("split"));
    s.execute("stills.delete", json!({"still": id})).unwrap();
    let r = s.execute("compare.get", json!({})).unwrap();
    assert_eq!((r["mode"].clone(), r["still"].clone(), r["active"].clone()), (json!("off"), Value::Null, json!(false)));
    // clearing the still turns the comparison off
    let id = grab(&mut s, json!({}));
    s.execute("compare.set", json!({"still": id, "mode": "wipe"})).unwrap();
    s.execute("compare.set", json!({"still": null})).unwrap();
    assert!(!s.state.compare.active());
}

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("fc-stills-{tag}-{}-{n:x}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn nonce_of(s: &Session, id: u64) -> u64 {
    s.project.stills.iter().find(|x| x.id == id).unwrap().nonce
}

#[test]
fn a_grab_after_undo_gets_its_own_picture_and_nonce() {
    let (mut s, a, _) = demo();
    let first = grab(&mut s, json!({}));
    let old = s.still_picture(first).unwrap();
    let old_nonce = nonce_of(&s, first);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.stills.is_empty());
    add_node(&mut s, a, 0.0);
    let second = grab(&mut s, json!({}));
    assert_eq!(first, second, "the id comes back after an undo");
    assert!(nonce_of(&s, second) > old_nonce, "but not the nonce");
    let new = s.still_picture(second).unwrap();
    assert_ne!(old.rgba, new.rgba, "the new grab shows the new grade, not the old file");
}

#[test]
fn an_unwritable_folder_means_no_picture_not_a_panic() {
    let (mut s, _, _) = demo();
    let d = scratch_dir("ro");
    let file = d.join("a-file");
    std::fs::write(&file, b"x").unwrap();
    s.state.stills_tmp = Some(file.join("sub"));
    let r = s.execute("stills.grab", json!({})).unwrap();
    assert_eq!(r["picture"], json!(false));
    let id = r["still"].as_u64().unwrap();
    assert!(s.still_picture(id).is_some(), "falls back to a re-render");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn hostile_pictures_are_not_decoded() {
    let d = scratch_dir("png");
    let ok = d.join("ok.png");
    image::RgbaImage::from_pixel(8, 4, image::Rgba([1, 2, 3, 255])).save(&ok).unwrap();
    assert!(super::stills::read_picture(&ok).is_some());
    let wide = d.join("wide.png");
    image::RgbaImage::from_pixel(5000, 2, image::Rgba([1, 2, 3, 255])).save(&wide).unwrap();
    assert!(super::stills::read_picture(&wide).is_none(), "wider than a still can be");
    let bytes = std::fs::read(&ok).unwrap();
    let cut = d.join("cut.png");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    assert!(super::stills::read_picture(&cut).is_none(), "truncated");
    let junk = d.join("junk.png");
    std::fs::write(&junk, b"not a png at all").unwrap();
    assert!(super::stills::read_picture(&junk).is_none());
    assert!(super::stills::read_picture(&d.join("missing.png")).is_none());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn stills_move_with_the_project_and_follow_scratch_disks() {
    let (mut s, _, _) = demo();
    let id = grab(&mut s, json!({}));
    let tmp = s.state.stills_tmp.clone().unwrap();
    assert!(tmp.join(format!("still-{id}.png")).is_file());
    // Save: the temp folder moves next to the project
    let root = scratch_dir("move");
    let a = root.join("a").join("Proj A.fcproj");
    std::fs::create_dir_all(a.parent().unwrap()).unwrap();
    s.path = Some(a.to_string_lossy().to_string());
    s.stills_follow_path();
    let at_a = root.join("a").join("FilmCraft Stills").join("Proj A");
    assert!(at_a.join(format!("still-{id}.png")).is_file());
    assert!(!tmp.exists(), "our temp folder is gone");
    assert!(s.state.stills_tmp.is_none());
    assert!(s.still_picture(id).is_some());
    // Save As elsewhere copies; the first project keeps its pictures
    let b = root.join("b").join("Proj B.fcproj");
    std::fs::create_dir_all(b.parent().unwrap()).unwrap();
    s.path = Some(b.to_string_lossy().to_string());
    s.stills_follow_path();
    assert!(root.join("b").join("FilmCraft Stills").join("Proj B").join(format!("still-{id}.png")).is_file());
    assert!(at_a.join(format!("still-{id}.png")).is_file());
    // Scratch Disks > Video Previews relocates them too
    let scratch = root.join("scratch");
    s.execute("file.projectSettings.scratchDisks", json!({"videoPreviews": scratch.to_string_lossy()})).unwrap();
    assert!(scratch.join("Stills").join("Proj B").join(format!("still-{id}.png")).is_file());
    assert_eq!(s.stills_dirs()[0], scratch.join("Stills").join("Proj B"));
    assert!(s.still_picture(id).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn closing_an_unsaved_project_deletes_its_temp_stills() {
    let (mut s, _, _) = demo();
    grab(&mut s, json!({}));
    let tmp = s.state.stills_tmp.clone().unwrap();
    assert!(tmp.is_dir());
    s.execute("file.closeProject", json!({"force": true})).unwrap();
    assert!(!tmp.exists());
}

#[test]
fn copy_grade_is_journaled_for_key_binding() {
    let spec = crate::commands::find("grade.copy").unwrap();
    assert!(spec.journal);
}

#[test]
fn a_hostile_still_nonce_does_not_overflow() {
    let (mut s, a, _) = demo();
    add_node(&mut s, a, 0.0);
    grab(&mut s, json!({"clip": a.0}));
    let mut p = (*s.project).clone();
    for st in &mut p.stills {
        st.nonce = u64::MAX;
    }
    s.project = std::sync::Arc::new(p);
    grab(&mut s, json!({"clip": a.0}));
    assert!(s.project.stills.iter().all(|st| st.nonce == u64::MAX));
}

#[test]
fn opening_another_project_discards_the_temporary_stills_folder() {
    let (mut s, a, _) = demo();
    add_node(&mut s, a, 0.0);
    grab(&mut s, json!({"clip": a.0}));
    let tmp = s.state.stills_tmp.clone();
    s.execute("file.openDemoProject", json!({})).unwrap();
    if let Some(dir) = tmp {
        assert!(!dir.exists(), "{} left behind", dir.display());
    }
    assert!(s.state.stills_tmp.is_none());
}

// ---- REF.1 / REF.3: reference pictures ----

/// Write a `w × h` PNG filled by `f(x, y)` into a scratch folder.
fn write_png(tag: &str, w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> std::path::PathBuf {
    let p = scratch_dir(tag).join(format!("{tag}.png"));
    image::RgbaImage::from_fn(w, h, |x, y| image::Rgba(f(x, y))).save(&p).unwrap();
    p
}

#[test]
fn import_reference_picture_is_a_still_without_a_grade() {
    let (mut s, a, _) = demo();
    let p = write_png("ref-teal", 2400, 1000, |x, _| [20, 90 + (x % 50) as u8, 110, 255]);
    let undo = s.history.undo.len();
    let r = s.execute("stills.import", json!({"path": p.to_string_lossy()})).unwrap();
    let id = r["still"].as_u64().unwrap();
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    // stored no wider than a grab, aspect kept
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(1920), Some(800)), "{r}");
    let st = s.project.stills.iter().find(|x| x.id == id).unwrap();
    assert!(st.is_reference() && st.grade.is_none());
    assert_eq!(st.label, "ref-teal");
    let pic = s.still_picture(id).unwrap();
    assert_eq!((pic.w, pic.h), (1920, 800));
    let list = s.execute("stills.list", json!({})).unwrap();
    assert_eq!(list["stills"][0]["reference"], json!(true), "{list}");
    assert_eq!(list["stills"][0]["picture"], json!(true), "{list}");
    // compare against it like any still; never apply it as a grade
    s.execute("compare.set", json!({"still": id, "mode": "wipe"})).unwrap();
    let before = grade(&s, a);
    assert!(s.execute("stills.apply", json!({"still": id, "clips": [a.0]})).is_err());
    assert_eq!(grade(&s, a), before);
    // a label of our own
    let r = s.execute("stills.import", json!({"path": p.to_string_lossy(), "label": "Look\nA"})).unwrap();
    let id2 = r["still"].as_u64().unwrap();
    assert_eq!(s.project.stills.iter().find(|x| x.id == id2).unwrap().label, "LookA");
}

#[test]
fn import_refuses_hostile_and_odd_files() {
    let (mut s, _, _) = demo();
    let d = scratch_dir("ref-bad");
    let write = |name: &str, b: &[u8]| {
        let p = d.join(name);
        std::fs::write(&p, b).unwrap();
        p.to_string_lossy().into_owned()
    };
    // a PNG header claiming 100 000 × 100 000 px (refused before any allocation)
    let mut huge = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    huge.extend_from_slice(&100_000u32.to_be_bytes());
    huge.extend_from_slice(&100_000u32.to_be_bytes());
    huge.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    let cases = [
        json!({}),
        json!({"path": 5}),
        json!({"path": d.join("missing.png").to_string_lossy()}),
        json!({"path": write("notes.txt", b"hello, not a picture")}),
        json!({"path": write("cut.jpg", &[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F'])}),
        json!({"path": write("cut.png", &b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0"[..])}),
        json!({"path": write("huge.png", &huge)}),
        json!({"path": write("tiff.tif", b"II*\0\x08\0\0\0\xff\xff")}),
        json!({"path": d.to_string_lossy()}),
        // an RLE Targa header declaring 60 000 × 60 000 px (refused before decoding)
        json!({"path": write("huge.tga", &[0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x60, 0xEA, 0x60, 0xEA, 32, 8, 0x80, 1, 2, 3, 4])}),
    ];
    let (undo, redo) = (s.history.undo.len(), s.history.redo.len());
    for c in cases {
        assert!(s.execute("stills.import", c.clone()).is_err(), "{c}");
    }
    assert!(s.project.stills.is_empty());
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo), "a refused import leaves no undo or redo step");
    let left: Vec<_> = s.stills_dirs().iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten().map(|e| e.file_name()).collect();
    assert!(left.is_empty(), "nothing left in the stills folder: {left:?}");
    // a 1 × 1 picture is fine
    let p = write_png("ref-dot", 1, 1, |_, _| [255, 0, 0, 255]);
    s.execute("stills.import", json!({"path": p.to_string_lossy()})).unwrap();
}

#[test]
fn shot_match_on_a_reference_still_moves_the_clip_toward_it() {
    let (mut s, a, _) = demo();
    // an orange / teal reference: warm subject, cool background
    let p = write_png("ref-look", 640, 360, |x, y| if (200..440).contains(&x) && (60..300).contains(&y) { [230, 150, 90, 255] } else { [30, 80, 95, 255] });
    let id = s.execute("stills.import", json!({"path": p.to_string_lossy()})).unwrap()["still"].as_u64().unwrap();
    let cmp0 = s.execute("grade.compareShots", json!({"clip": a.0, "still": id})).unwrap();
    let undo = s.history.undo.len();
    let m = s.execute("grade.shotMatch", json!({"clip": a.0, "still": id, "faceDetection": false})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    assert_eq!(grade(&s, a).unwrap().nodes.last().map(|n| n.label.as_str()), Some("Shot Match"));
    let (before, after) = (m["before"].as_f64().unwrap(), m["after"].as_f64().unwrap());
    assert!(after < before, "the match moves toward the reference: {m}");
    let cmp1 = s.execute("grade.compareShots", json!({"clip": a.0, "still": id})).unwrap();
    assert_ne!(cmp0, cmp1, "the comparison changes after the match");
    // bad still ids
    for bad in [json!("x"), json!(987_654_321u64)] {
        assert!(s.execute("grade.shotMatch", json!({"clip": a.0, "still": bad})).is_err());
        assert!(s.execute("grade.compareShots", json!({"clip": a.0, "still": bad})).is_err());
    }
}

/// Fork C2 review: Shot Match on a branched grade wires its node in after the output, so the
/// grade still renders (a bare node with no connections made the output ambiguous).
#[test]
fn shot_match_on_a_branched_grade_wires_in_after_the_output() {
    use filmcraft_project::dag::{Edge, Port};
    let (mut s, a, _) = demo();
    let n = s.execute("grade.addNode", json!({"clip": a.0})).unwrap()["node"].as_u64().unwrap();
    let m = s.execute("grade.addParallel", json!({"clip": a.0, "node": n})).unwrap()["nodes"][1].as_u64().unwrap();
    let p = write_png("ref-branch", 64, 36, |x, _| if x < 32 { [230, 150, 90, 255] } else { [30, 80, 95, 255] });
    let id = s.execute("stills.import", json!({"path": p.to_string_lossy()})).unwrap()["still"].as_u64().unwrap();
    let sm = s.execute("grade.shotMatch", json!({"clip": a.0, "still": id, "faceDetection": false})).unwrap()["node"].as_u64().unwrap();
    let g = grade(&s, a).unwrap();
    assert!(g.edges.contains(&Edge::new(Port::new(m, 0), Port::new(sm, 0))), "{:?}", g.edges);
    assert_eq!(g.output_node(), Ok(sm));
    assert!(g.graph_order().is_ok());
}
