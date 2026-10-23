//! `comp.template.*` (fork C4.4): save → list → insert → delete, undo, hostile names and files.

use std::sync::Arc;

use filmcraft_project::ItemKind;
use filmcraft_project::comp::Comp;
use serde_json::{Value, json};

use super::safe_name;
use crate::Session;
use crate::media_test_util::tmp_dir;

fn demo(dir: &str) -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.prefs_path = Some(tmp_dir(dir).join("prefs.json"));
    s
}

fn media(s: &Session) -> u64 {
    s.project.items.values().find(|i| matches!(i.kind, ItemKind::Media(_)) && i.has_video()).unwrap().id.0
}

fn add(s: &mut Session, comp: u64, mut p: Value) -> u64 {
    p["comp"] = json!(comp);
    s.execute("comp.addNode", p).unwrap()["node"].as_u64().unwrap()
}

fn connect(s: &mut Session, comp: u64, from: u64, to: u64, port: u8) {
    s.execute("comp.connect", json!({"comp": comp, "from": from, "to": to, "toPort": port})).unwrap();
}

fn comp(s: &Session, id: u64) -> Comp {
    match &s.project.item(filmcraft_project::ItemId(id)).unwrap().kind {
        ItemKind::Comp(c) => (**c).clone(),
        _ => panic!("not a comp"),
    }
}

/// Media In (item) → Key → Merge over a Solid → Color → Out; returns (comp, [mediaIn, key, solid, merge, color]).
fn build(s: &mut Session) -> (u64, [u64; 5]) {
    let item = media(s);
    let c = s.execute("comp.new", json!({"name": "A"})).unwrap()["comp"].as_u64().unwrap();
    let m = add(s, c, json!({"kind": "mediaIn", "item": item}));
    let k = add(s, c, json!({"kind": "key", "effect": "luma_key", "pos": [100, 0]}));
    let so = add(s, c, json!({"kind": "solid", "pos": [100, 200]}));
    let mg = add(s, c, json!({"kind": "merge", "pos": [300, 100]}));
    let col = add(s, c, json!({"kind": "color", "pos": [500, 100]}));
    connect(s, c, m, k, 0);
    connect(s, c, so, mg, 0);
    connect(s, c, k, mg, 1);
    connect(s, c, mg, col, 0);
    connect(s, c, col, 1, 0);
    s.execute("comp.setParam", json!({"comp": c, "node": mg, "param": "opacity", "value": 40.0})).unwrap();
    (c, [m, k, so, mg, col])
}

fn names(s: &mut Session) -> Vec<String> {
    let v = s.execute("comp.template.list", json!({})).unwrap();
    v["templates"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn save_list_insert_undo_delete() {
    let mut s = demo("ctmpl-flow");
    let (c, [m, k, so, mg, col]) = build(&mut s);
    let v = s.execute("comp.template.save", json!({"comp": c, "nodes": [m, k, so, mg, col, 1], "name": "Key Over Solid"})).unwrap();
    assert_eq!(v["nodes"], 4);
    assert_eq!(v["droppedMediaIn"], 1);
    assert_eq!(v["droppedMediaOut"], 1);
    assert!(v["message"].as_str().unwrap().contains("left out 1 Media In node that reads a project item"), "{v}");
    let path = std::path::PathBuf::from(v["path"].as_str().unwrap());
    assert!(path.ends_with("templates/comp/Key Over Solid.fctemplate"), "{}", path.display());
    // no temporary files left behind
    let files: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(files.len(), 1, "{files:?}");

    let list = s.execute("comp.template.list", json!({})).unwrap();
    let t = &list["templates"][0];
    assert_eq!((t["name"].as_str(), t["nodes"].as_u64(), t["edges"].as_u64()), (Some("Key Over Solid"), Some(4), Some(3)));
    assert!(t["modified"].as_u64().is_some());

    // insert into a fresh comp: one undo step, fresh ids, the same graph
    let c2 = s.execute("comp.new", json!({"name": "B"})).unwrap()["comp"].as_u64().unwrap();
    let before = comp(&s, c2);
    let undo = s.history.undo.len();
    let v = s.execute("comp.template.insert", json!({"comp": c2, "name": "Key Over Solid", "at": [50, 60]})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    let ids: Vec<u64> = v["nodes"].as_array().unwrap().iter().map(|n| n.as_u64().unwrap()).collect();
    assert_eq!(ids, vec![2, 3, 4, 5]);
    let after = comp(&s, c2);
    let (nk, nso, nmg, ncol) = (ids[0], ids[1], ids[2], ids[3]);
    assert_eq!(after.node(nk).unwrap().pos, [50.0, 60.0]);
    assert_eq!(after.node(nso).unwrap().pos, [50.0, 260.0]);
    let orig = comp(&s, c);
    for (a, b) in [(k, nk), (so, nso), (mg, nmg), (col, ncol)] {
        let (a, b) = (orig.node(a).unwrap(), after.node(b).unwrap());
        assert_eq!((&a.kind, &a.params, &a.label, a.bypass), (&b.kind, &b.params, &b.label, b.bypass));
    }
    // render-equivalent: wired to the output, the inserted graph evaluates in the same shape
    s.execute("comp.connect", json!({"comp": c2, "from": ncol, "to": 1, "toPort": 0})).unwrap();
    let order: Vec<u64> = comp(&s, c2).validate().unwrap().nodes().collect();
    assert_eq!(order, vec![nk, nso, nmg, ncol, 1]);
    let orig_order: Vec<u64> = orig.validate().unwrap().nodes().collect();
    assert_eq!(orig_order, vec![m, k, so, mg, col, 1]);

    // undo the connect and the insert: back to the empty comp
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(comp(&s, c2), before);

    // inserting twice gives distinct ids; default position below the existing nodes
    let a = s.execute("comp.template.insert", json!({"comp": c, "name": "Key Over Solid"})).unwrap();
    let a: Vec<u64> = a["nodes"].as_array().unwrap().iter().map(|n| n.as_u64().unwrap()).collect();
    assert_eq!(a, vec![7, 8, 9, 10]);
    assert!(comp(&s, c).node(7).unwrap().pos[1] > 200.0);

    // delete: file only
    s.execute("comp.template.delete", json!({"name": "Key Over Solid"})).unwrap();
    assert!(names(&mut s).is_empty());
    assert!(!path.exists());
    assert_eq!(comp(&s, c).nodes.len(), 10);
    let e = s.execute("comp.template.delete", json!({"name": "Key Over Solid"})).unwrap_err().to_string();
    assert!(e.contains("no template called"), "{e}");
    let e = s.execute("comp.template.insert", json!({"comp": c, "name": "Key Over Solid"})).unwrap_err().to_string();
    assert!(e.contains("no template called"), "{e}");
}

#[test]
fn overwrite_needs_replace() {
    let mut s = demo("ctmpl-replace");
    let (c, [_, k, so, mg, _]) = build(&mut s);
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Look"})).unwrap();
    let e = s.execute("comp.template.save", json!({"comp": c, "nodes": [k, so, mg], "name": "Look"})).unwrap_err().to_string();
    assert!(e.contains("already exists"), "{e}");
    assert_eq!(s.execute("comp.template.list", json!({})).unwrap()["templates"][0]["nodes"], 1);
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k, so, mg], "name": "Look", "replace": true})).unwrap();
    assert_eq!(s.execute("comp.template.list", json!({})).unwrap()["templates"][0]["nodes"], 3);
}

#[test]
fn hostile_names_are_refused_and_unicode_is_fine() {
    for bad in
        ["", "   ", "../x", "..", ".hidden", "a/b", "a\\b", "x\u{0}y", "tab\there", "CON", "con.txt", "Lpt1", "nul ", "a:b", "end.", "x".repeat(65).as_str()]
    {
        assert!(safe_name(bad).is_err(), "{bad:?}");
    }
    assert_eq!(safe_name("  Glow  ").unwrap(), "Glow");
    assert_eq!(safe_name("Console").unwrap(), "Console");

    let mut s = demo("ctmpl-names");
    let (c, [_, k, ..]) = build(&mut s);
    let dir = super::templates_dir(&s).unwrap();
    for bad in ["../x", "a/b", "CON", "..\\..\\evil", ""] {
        let undo = s.history.undo.len();
        let e = s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": bad})).unwrap_err().to_string();
        assert!(!e.is_empty());
        assert_eq!(s.history.undo.len(), undo);
        assert!(s.execute("comp.template.insert", json!({"comp": c, "name": bad})).is_err());
        assert!(s.execute("comp.template.delete", json!({"name": bad})).is_err());
    }
    // nothing was written anywhere near the data folder
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());
    assert!(!dir.parent().unwrap().join("x.fctemplate").exists());
    assert!(!dir.parent().unwrap().parent().unwrap().join("x.fctemplate").exists());

    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "ルック 日本 ✨"})).unwrap();
    assert_eq!(names(&mut s), vec!["ルック 日本 ✨".to_string()]);
    s.execute("comp.template.insert", json!({"comp": c, "name": "ルック 日本 ✨"})).unwrap();
}

#[test]
fn damaged_files_are_skipped_and_refused() {
    let mut s = demo("ctmpl-damaged");
    let (c, [_, k, ..]) = build(&mut s);
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Good"})).unwrap();
    let dir = super::templates_dir(&s).unwrap();
    std::fs::write(dir.join("Garbage.fctemplate"), b"\xff not json").unwrap();
    std::fs::write(dir.join("Newer.fctemplate"), br#"{"version":99,"name":"x","nodes":[{"id":1,"kind":{"type":"blur"}}]}"#).unwrap();
    std::fs::write(dir.join("Big.fctemplate"), vec![b' '; 5 * 1024 * 1024]).unwrap();
    std::fs::write(dir.join("Item.fctemplate"), br#"{"version":1,"name":"x","nodes":[{"id":1,"kind":{"type":"mediaIn","source":{"kind":"item","item":1}}}]}"#)
        .unwrap();
    std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
    std::fs::create_dir_all(dir.join("Folder.fctemplate")).unwrap();
    assert_eq!(names(&mut s), vec!["Good".to_string()]);
    for (name, want) in
        [("Garbage", "can't be read"), ("Newer", "newer version"), ("Big", "larger than 4 MiB"), ("Item", "project item"), ("Folder", "not a file")]
    {
        let (proj, undo) = (s.project.clone(), s.history.undo.len());
        let e = s.execute("comp.template.insert", json!({"comp": c, "name": name})).unwrap_err().to_string();
        assert!(e.contains(want), "{name}: {e}");
        assert!(Arc::ptr_eq(&proj, &s.project));
        assert_eq!(s.history.undo.len(), undo);
    }
}

#[test]
fn bad_params_and_no_data_folder() {
    let mut s = demo("ctmpl-params");
    let (c, [_, k, ..]) = build(&mut s);
    for p in [
        json!({"comp": c, "name": "x"}),
        json!({"comp": c, "nodes": "1", "name": "x"}),
        json!({"comp": c, "nodes": [-1], "name": "x"}),
        json!({"comp": c, "nodes": [], "name": "x"}),
        json!({"comp": c, "nodes": [999], "name": "x"}),
        json!({"comp": c, "nodes": [1], "name": "x"}),
        json!({"comp": c, "nodes": [k]}),
        json!({"comp": 999_999, "nodes": [k], "name": "x"}),
    ] {
        assert!(s.execute("comp.template.save", p.clone()).is_err(), "{p}");
    }
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "x"})).unwrap();
    for at in [json!([1]), json!(["a", 1]), json!([1e300, 0]), json!("here")] {
        assert!(s.execute("comp.template.insert", json!({"comp": c, "name": "x", "at": at})).is_err(), "{at}");
    }
    s.prefs_path = None;
    assert_eq!(s.execute("comp.template.list", json!({})).unwrap()["templates"], json!([]));
    let e = s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "y"})).unwrap_err().to_string();
    assert!(e.contains("no data folder"), "{e}");
}

#[test]
fn insert_refuses_a_full_comp() {
    let mut s = demo("ctmpl-full");
    let (c, [_, k, so, mg, _]) = build(&mut s);
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k, so, mg], "name": "Three"})).unwrap();
    while comp(&s, c).nodes.len() < filmcraft_project::dag::MAX_NODES - 1 {
        add(&mut s, c, json!({"kind": "solid"}));
    }
    let (proj, undo) = (s.project.clone(), s.history.undo.len());
    let e = s.execute("comp.template.insert", json!({"comp": c, "name": "Three"})).unwrap_err().to_string();
    assert!(e.contains("at most"), "{e}");
    assert!(Arc::ptr_eq(&proj, &s.project));
    assert_eq!(s.history.undo.len(), undo);
}

#[test]
fn more_reserved_names_are_refused() {
    for bad in ["con.txt", "COM1.x", "com0", "LPT0", "conin$", "CONOUT$.fctemplate", "com\u{b9}", "lpt\u{b3}.a", "Aux"] {
        assert!(safe_name(bad).is_err(), "{bad:?}");
    }
    for ok in ["com10", "computer", "lpt", "comet.1", "console"] {
        assert!(safe_name(ok).is_ok(), "{ok:?}");
    }
}

#[test]
fn names_differing_only_in_case_conflict() {
    let mut s = demo("ctmpl-case");
    let (c, [_, k, so, ..]) = build(&mut s);
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Look"})).unwrap();
    let e = s.execute("comp.template.save", json!({"comp": c, "nodes": [k, so], "name": "look"})).unwrap_err().to_string();
    assert!(e.contains("\u{201c}Look\u{201d} already exists"), "{e}");
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k, so], "name": "look", "replace": true})).unwrap();
    assert_eq!(names(&mut s), vec!["look".to_string()]);
    assert_eq!(s.execute("comp.template.list", json!({})).unwrap()["templates"][0]["nodes"], 2);
    // the exact name exists too: it is overwritten and the other spelling left alone
    let dir = super::templates_dir(&s).unwrap();
    std::fs::copy(dir.join("look.fctemplate"), dir.join("LOOK.fctemplate")).unwrap();
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "look", "replace": true})).unwrap();
    assert_eq!(names(&mut s), vec!["LOOK".to_string(), "look".to_string()]);
    // two other spellings: neither is deleted
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Look", "replace": true})).unwrap();
    assert_eq!(names(&mut s), vec!["LOOK".to_string(), "Look".to_string(), "look".to_string()]);
    // names only: no counts
    let v = s.execute("comp.template.list", json!({"namesOnly": true})).unwrap();
    assert_eq!(v["templates"][0], json!({"name": "LOOK"}));
    // file commands are not journalled (replay can't depend on the templates folder)
    for id in ["comp.template.save", "comp.template.insert", "comp.template.delete"] {
        assert!(crate::commands::command_specs().iter().any(|c| c.id == id && !c.journal), "{id}");
    }
}

#[cfg(unix)]
#[test]
fn links_sparse_files_and_temporaries() {
    use std::os::unix::fs::symlink;
    let mut s = demo("ctmpl-links");
    let (c, [_, k, ..]) = build(&mut s);
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Good"})).unwrap();
    let dir = super::templates_dir(&s).unwrap();
    let good = dir.join("Good.fctemplate");
    // a link to a template in the folder and a link to a file outside it: neither is read
    symlink(&good, dir.join("Inside.fctemplate")).unwrap();
    let outside = tmp_dir("ctmpl-outside").join("secret.fctemplate");
    std::fs::copy(&good, &outside).unwrap();
    symlink(&outside, dir.join("Outside.fctemplate")).unwrap();
    // a sparse file far over the cap
    let f = std::fs::File::create(dir.join("Sparse.fctemplate")).unwrap();
    f.set_len(64 * 1024 * 1024 * 1024).unwrap();
    drop(f);
    // a fresh leftover temporary file (kept, not listed) and a stale one (swept)
    let fresh = dir.join(".Good.fctemplate.tmp-1-1");
    let stale = dir.join(".Good.fctemplate.tmp-2-2");
    std::fs::write(&fresh, b"partial").unwrap();
    std::fs::write(&stale, b"partial").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    std::fs::File::options().write(true).open(&stale).unwrap().set_modified(old).unwrap();

    assert_eq!(names(&mut s), vec!["Good".to_string()]);
    assert!(fresh.exists() && !stale.exists());
    for (name, want) in [("Inside", "link"), ("Outside", "link"), ("Sparse", "larger than 4 MiB")] {
        let e = s.execute("comp.template.insert", json!({"comp": c, "name": name})).unwrap_err().to_string();
        assert!(e.contains(want), "{name}: {e}");
    }
    // delete refuses links (so never follows one)
    assert!(s.execute("comp.template.delete", json!({"name": "Outside"})).is_err());
    assert!(outside.exists());
    // a folder named like a template, and a missing name
    std::fs::create_dir_all(dir.join("x.fctemplate")).unwrap();
    for name in ["x", "Missing"] {
        let e = s.execute("comp.template.delete", json!({"name": name})).unwrap_err().to_string();
        assert!(e.contains("no template called"), "{name}: {e}");
    }
    assert!(dir.join("x.fctemplate").is_dir());
}

#[test]
fn list_cap_counts_templates_not_other_files() {
    let mut s = demo("ctmpl-cap");
    let (c, [_, k, ..]) = build(&mut s);
    let dir = super::templates_dir(&s).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..(super::MAX_LISTED + 500) {
        std::fs::write(dir.join(format!("note{i}.txt")), b"x").unwrap();
    }
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Zed"})).unwrap();
    assert_eq!(names(&mut s), vec!["Zed".to_string()]);
}

/// LO.4 (C4-TESTS): `comp.template.list` reads at most [`super::LIST_BUDGET`] bytes of templates;
/// past it the rest are listed by name with unknown counts (no file read).
#[test]
fn list_stops_reading_at_its_byte_budget() {
    let mut s = demo("ctmpl-budget");
    let (c, [_, k, ..]) = build(&mut s);
    s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Zed"})).unwrap();
    let dir = super::templates_dir(&s).unwrap();
    let mut big = std::fs::read(super::path_of(&dir, "Zed")).unwrap();
    // a valid template padded with JSON whitespace to just under the 4 MiB file cap
    let size = filmcraft_project::comp_template::MAX_BYTES - 1;
    assert!(big.len() < size);
    big.resize(size, b' ');
    for i in 0..20 {
        std::fs::write(super::path_of(&dir, &format!("T{i:02}")), &big).unwrap();
    }
    let v = s.execute("comp.template.list", json!({})).unwrap();
    let list = v["templates"].as_array().unwrap();
    assert_eq!(list.len(), 21, "{v}");
    // 16 files leave 16 bytes; the 17th is read and spends the rest; then names only
    let read = (super::LIST_BUDGET / size as u64 + 1) as usize;
    assert_eq!(read, 17);
    for (i, t) in list.iter().enumerate() {
        assert_eq!(t["nodes"].is_null(), i >= read, "{i}: {t}");
    }
    assert_eq!(list[20]["name"], "Zed");
    for t in list {
        std::fs::remove_file(super::path_of(&dir, t["name"].as_str().unwrap())).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn a_failed_save_leaves_no_temporary_file() {
    use std::os::unix::fs::PermissionsExt;
    let mut s = demo("ctmpl-readonly");
    let (c, [_, k, ..]) = build(&mut s);
    let dir = super::templates_dir(&s).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = s.execute("comp.template.save", json!({"comp": c, "nodes": [k], "name": "Ro"}));
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // (root can write anyway; then the save simply worked)
    if let Err(e) = r {
        assert!(e.to_string().contains("could not"), "{e}");
        assert!(left.is_empty(), "{left:?}");
    }
}

fn picture(c: &Comp) -> Vec<f32> {
    use filmcraft_render::comp::{CompCtx, NoSources, render_cpu};
    render_cpu(c, CompCtx { t: filmcraft_time::Tick(0), size: (64, 36), project: None, high_quality: false }, &NoSources).px
}

/// C4.3 groups: a Group node (with an exposed parameter set) saved as a template and inserted
/// into another comp renders the same picture; one that reads a project item inside loses it.
#[test]
fn a_group_template_renders_like_the_original_group() {
    let mut s = demo("ctmpl-group");
    let c = s.execute("comp.new", json!({"name": "G", "width": 64, "height": 36})).unwrap()["comp"].as_u64().unwrap();
    let g = add(&mut s, c, json!({"kind": "gradient"}));
    let b = add(&mut s, c, json!({"kind": "blur"}));
    let t = add(&mut s, c, json!({"kind": "transform"}));
    connect(&mut s, c, g, b, 0);
    connect(&mut s, c, b, t, 0);
    connect(&mut s, c, t, 1, 0);
    s.execute("comp.setParam", json!({"comp": c, "node": t, "param": "rotation", "value": 20.0})).unwrap();
    let grp = s.execute("comp.group", json!({"comp": c, "nodes": [g, b, t], "label": "Look"})).unwrap()["node"].as_u64().unwrap();
    let exposed = s.execute("comp.group.expose", json!({"comp": c, "node": grp, "inner": b, "param": "blurriness"})).unwrap();
    let id = exposed["id"].as_str().map(str::to_string).unwrap_or_else(|| "blurriness".into());
    s.execute("comp.setParam", json!({"comp": c, "node": grp, "param": id, "value": 6.0})).unwrap();
    let want = picture(&comp(&s, c));

    let v = s.execute("comp.template.save", json!({"comp": c, "nodes": [grp], "name": "Group Look"})).unwrap();
    assert_eq!((v["nodes"].as_u64(), v["droppedMediaIn"].as_u64()), (Some(1), Some(0)));
    let c2 = s.execute("comp.new", json!({"name": "H", "width": 64, "height": 36})).unwrap()["comp"].as_u64().unwrap();
    let ids = s.execute("comp.template.insert", json!({"comp": c2, "name": "Group Look"})).unwrap()["nodes"].clone();
    let new = ids[0].as_u64().unwrap();
    connect(&mut s, c2, new, 1, 0);
    let got = comp(&s, c2);
    let (a, b2) = (comp(&s, c).node(grp).unwrap().clone(), got.node(new).unwrap().clone());
    // the group's inside keeps its own ids (scoped to the group); only the group node is renumbered
    assert_eq!((&a.kind, &a.params, &a.label), (&b2.kind, &b2.params, &b2.label));
    assert_eq!(picture(&got), want);

    // a group that reads a project item inside: that Media In is left out and counted
    let item = media(&s);
    let c3 = s.execute("comp.new", json!({"name": "I", "width": 64, "height": 36})).unwrap()["comp"].as_u64().unwrap();
    let m = add(&mut s, c3, json!({"kind": "mediaIn", "item": item}));
    let bl = add(&mut s, c3, json!({"kind": "blur"}));
    connect(&mut s, c3, m, bl, 0);
    connect(&mut s, c3, bl, 1, 0);
    let grp3 = s.execute("comp.group", json!({"comp": c3, "nodes": [m, bl]})).unwrap()["node"].as_u64().unwrap();
    let v = s.execute("comp.template.save", json!({"comp": c3, "nodes": [grp3], "name": "Clip Look"})).unwrap();
    assert_eq!(v["droppedMediaIn"], 1, "{v}");
    assert!(v["message"].as_str().unwrap().contains("left out 1 Media In node"), "{v}");
    let text = std::fs::read_to_string(super::templates_dir(&s).unwrap().join("Clip Look.fctemplate")).unwrap();
    assert!(!text.contains("\"item\""), "{text}");
    s.execute("comp.template.insert", json!({"comp": c2, "name": "Clip Look"})).unwrap();
}
