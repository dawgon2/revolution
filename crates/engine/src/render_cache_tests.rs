//! The render cache (fork 6.3): part grid and names, the hash following edits / undo / relink,
//! queue order, eviction, the bounded index and file reads, the worker waiting while playing, and
//! playback picking (and pre-opening) cached parts.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use filmcraft_media::Generator;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_project::{ClipId, ItemId, ItemKind, Label, MediaRef, SequenceSettings, TrackKind, find_effect};
use filmcraft_render::preview::Need;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::json;

use super::*;
use crate::Session;
use crate::background::Activity;
use crate::previews::BarState;

fn tmp(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("filmcraft-render-cache-test-{name}-{}-{nanos}", std::process::id()))
}

/// A `w`×`h` 24 fps sequence, V1 = three 3 s clips of one matte: [Gaussian Blur + Lumetri +
/// Sharpen (red at 1080p, yellow when tiny), Tint (yellow), plain (no bar)].
fn session(name: &str, w: u32, h: u32) -> (Session, ItemId, Vec<ClipId>, ItemId) {
    let mut s = Session::default();
    s.previews.set_dir(Some(tmp(name)));
    let r = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let matte = crate::demo::add_generator(
        &mut p,
        &s.media,
        GeneratorSource::new(Generator::ColorMatte { color: [0.2, 0.5, 0.8, 1.0] }, w, h, r, Tick(20 * TICKS_PER_SECOND)),
        "Matte",
        Label::Iris,
        None,
    );
    let seq = p.new_sequence("Seq", SequenceSettings { width: w, height: h, frame_rate: r, ..Default::default() }, 1, 0, None);
    let mut items = Vec::new();
    for (i, fx) in [&["gaussian_blur", "lumetri", "sharpen"][..], &["tint"][..], &[][..]].iter().enumerate() {
        let at = r.tick_of(72 * i as i64);
        let mut it = p.make_track_item(matte, TrackKind::Video, at, TimeRange::new(at, r.tick_of(72)), r).unwrap();
        for e in it.effects.iter_mut() {
            filmcraft_project::resolve_auto_points(e, (w, h), (w, h));
        }
        for id in *fx {
            it.effects.push(find_effect(id).unwrap().instance());
        }
        items.push(it);
    }
    let ids: Vec<ClipId> = items.iter().map(|i| i.id).collect();
    p.sequence_mut(seq).unwrap().video_tracks[0].items = items;
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences = vec![seq];
    (s, seq, ids, matte)
}

fn hashes(s: &Session, seq: ItemId) -> Vec<String> {
    s.previews.segments(&s.project, seq).iter().map(|g| g.hash.clone()).collect()
}

fn edit_project(s: &mut Session, f: impl FnOnce(&mut filmcraft_project::Project)) {
    let mut p = (*s.project).clone();
    f(&mut p);
    s.project = Arc::new(p);
}

/// Tick and wait until the cache has nothing left to do (bounded).
fn settle(s: &mut Session) {
    for _ in 0..20 {
        tick(s, true);
        assert!(s.background.wait_idle(Duration::from_secs(60)), "the worker finishes");
        tick(s, true);
        if s.previews.cache.queued().is_empty() && s.background.is_idle() {
            return;
        }
    }
    panic!("the render cache did not settle");
}

fn parts(s: &Session) -> Vec<String> {
    s.previews.cache.index().keys().filter(|n| parse_video_name(n).is_some()).cloned().collect()
}

#[test]
fn part_grid_and_names_round_trip() {
    assert_eq!(part_frames(FrameRate::FPS_24), 48);
    assert_eq!(part_frames(FrameRate::FPS_29_97), 60);
    let h = "0123456789abcdef0123456789abcdef";
    for frames in [1i64, 47, 48, 49, 72, 100, 1000] {
        let cells = grid(frames, 48);
        assert_eq!(cells.first().map(|c| c.0), Some(0));
        assert_eq!(cells.iter().map(|c| c.1).sum::<i64>(), frames, "the grid covers the segment");
        for w in cells.windows(2) {
            assert_eq!(w[0].0 + w[0].1, w[1].0, "cells are contiguous");
        }
        for &(off, len) in &cells {
            let t = PartTodo { hash: h.into(), seg_first: 500, seg_frames: frames, off, len, tier: 0, distance: 0 };
            let name = t.name();
            let parsed = parse_video_name(&name).unwrap();
            if off == 0 && len == frames {
                assert_eq!(name, format!("{h}.mov"), "a short segment is one whole file");
                assert_eq!((parsed.0, parsed.1), (h, 0));
            } else {
                assert_eq!(parsed, (h, off, len), "{name}");
            }
            assert_eq!(owner_hash(&name), Some(h));
        }
    }
    assert!(grid(0, 48).is_empty());
    assert!(grid(-5, 48).is_empty());
    assert_eq!(grid(10, 0).len(), 10, "a zero part length counts as one frame");
    assert_eq!(Mode::parse("smart"), Some(Mode::Smart));
    assert_eq!(Mode::parse("bogus"), None);
    for (v, _) in MODES {
        assert_eq!(Mode::parse(v).map(Mode::as_str), Some(*v));
    }
}

/// The cache key is the segment hash: stable, changed by an edit, restored by undo, changed by
/// a relink, and kept when a proxy is attached.
#[test]
fn key_follows_edits_undo_and_relink() {
    let (mut s, seq, ids, matte) = session("key", 64, 36);
    let h0 = hashes(&s, seq);
    assert_eq!(h0.len(), 3);
    edit_project(&mut s, |_| {});
    assert_eq!(hashes(&s, seq), h0, "a new snapshot of the same content keeps every hash");
    // edit the Tint → only the second segment changes
    s.edit_sequence("Tint", |q, _, _| {
        let (_, it) = q.find_item_mut(ids[1]).unwrap();
        let p = it.effect_mut("tint").unwrap().params.values_mut().find(|p| matches!(p.value, filmcraft_project::ParamValue::Float(_))).unwrap();
        p.value = filmcraft_project::ParamValue::Float(42.0);
        Ok(())
    })
    .unwrap();
    let h1 = hashes(&s, seq);
    assert_eq!((h1[0] == h0[0], h1[1] == h0[1], h1[2] == h0[2]), (true, false, true));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(hashes(&s, seq), h0, "undo brings the old hash back");
    // a proxy keeps the hash (v2), a relink to another file changes it
    edit_project(&mut s, |p| {
        if let Some(ItemKind::Media(m)) = p.items.get_mut(&matte).map(|i| &mut i.kind) {
            m.proxy = Some(MediaRef::File { path: "/proxies/matte_Proxy.mov".into() });
        }
    });
    assert_eq!(hashes(&s, seq), h0, "attaching a proxy keeps the cache");
    edit_project(&mut s, |p| {
        if let Some(ItemKind::Media(m)) = p.items.get_mut(&matte).map(|i| &mut i.kind) {
            m.media = MediaRef::File { path: "/footage/other.mov".into() };
        }
    });
    let h2 = hashes(&s, seq);
    assert!(h2.iter().zip(&h0).all(|(a, b)| a != b), "a relink changes the source identity and every hash");
}

/// Smart mode wants red, yellow and graded segments, never a plain native clip; the queue order is
/// near the playhead, then red, then yellow, then the rest.
#[test]
fn smart_policy_and_queue_order() {
    let (mut s, seq, ids, _) = session("order", 1920, 1080);
    let segs = s.previews.segments(&s.project, seq);
    assert_eq!(segs.iter().map(|g| g.need).collect::<Vec<_>>(), [Need::Render, Need::Realtime, Need::None]);
    // Smart: the plain clip is not cached; playhead in the second clip, nothing counts as near
    let todo = plan(&s.previews, &s.project, seq, 100, Mode::Smart, 0);
    let order: Vec<(i64, u8)> = todo.iter().map(|t| (t.first_frame(), t.tier)).collect();
    assert_eq!(order, [(72, 0), (48, 1), (0, 1), (120, 2)], "playhead part, then red, then yellow (each nearest first)");
    assert!(todo.iter().all(|t| t.len > 0 && t.len <= 48));
    // All adds the plain clip last
    let todo = plan(&s.previews, &s.project, seq, 100, Mode::All, 0);
    assert_eq!(todo.iter().map(|t| (t.first_frame(), t.tier)).collect::<Vec<_>>(), [(72, 0), (48, 1), (0, 1), (120, 2), (144, 3), (192, 3)]);
    // within the near window, nearest first
    let todo = plan(&s.previews, &s.project, seq, 150, Mode::All, 24 * 30);
    let firsts: Vec<i64> = todo.iter().map(PartTodo::first_frame).collect();
    assert_eq!(firsts, [144, 120, 72, 192, 48, 0]);
    assert!(todo.iter().all(|t| t.tier == 0));
    assert!(plan(&s.previews, &s.project, seq, 0, Mode::Off, 0).is_empty());
    // a graded plain clip is always cached in Smart mode
    let node: filmcraft_project::grade::GradeNode = serde_json::from_value(json!({"id": 1})).unwrap();
    s.edit_sequence("grade", |q, _, _| {
        let (_, it) = q.find_item_mut(ids[2]).unwrap();
        it.grade = Some(filmcraft_project::grade::Grade { nodes: vec![node], bypass: false, ..Default::default() });
        Ok(())
    })
    .unwrap();
    let todo = plan(&s.previews, &s.project, seq, 0, Mode::Smart, 0);
    assert!(todo.iter().any(|t| t.first_frame() == 144), "graded: {todo:?}");
    // a bypassed grade is not
    s.edit_sequence("bypass", |q, _, _| {
        let (_, it) = q.find_item_mut(ids[2]).unwrap();
        if let Some(g) = it.grade.as_mut() {
            g.bypass = true;
        }
        Ok(())
    })
    .unwrap();
    assert!(!plan(&s.previews, &s.project, seq, 0, Mode::Smart, 0).iter().any(|t| t.first_frame() >= 144));
}

#[test]
fn eviction_unused_first_then_least_recently_used() {
    let h = |c: char| c.to_string().repeat(32);
    let (a, b, c) = (h('a'), h('b'), h('c'));
    let mut files = BTreeMap::new();
    let e = |last_used| IndexEntry { bytes: 100, last_used, project: None };
    files.insert(format!("{a}-0-48.mov"), e(50));
    files.insert(format!("{a}-48-48.mov"), e(10));
    files.insert(format!("{b}.mov"), e(99));
    files.insert(format!("{c}.wav"), e(1));
    files.insert(format!("{c}-0-48.mov"), e(5));
    let referenced: HashSet<String> = [a.clone(), c.clone()].into();
    // b is not used by any sequence: first despite being the most recent; then by last use
    assert_eq!(
        eviction_order(&files, &referenced),
        [format!("{b}.mov"), format!("{c}.wav"), format!("{c}-0-48.mov"), format!("{a}-48-48.mov"), format!("{a}-0-48.mov")]
    );
    let none = HashSet::new();
    assert!(evict(&files, &referenced, &none, 500).is_empty(), "within the budget");
    assert_eq!(evict(&files, &referenced, &none, 300), [format!("{b}.mov"), format!("{c}.wav")]);
    assert_eq!(evict(&files, &referenced, &none, 0).len(), 5);
    // the active sequence's parts are never evicted, even far over the budget
    let protected: HashSet<String> = [c.clone()].into();
    assert_eq!(evict(&files, &referenced, &protected, 0), [format!("{b}.mov"), format!("{a}-48-48.mov"), format!("{a}-0-48.mov")]);
    assert_eq!(low_water(1000), 900);
    assert_eq!(budget_bytes(0), 1 << 30, "at least 1 GB");
    assert_eq!(budget_bytes(20), 20 << 30);
}

/// Over the limit the session deletes files (unused first) and queues nothing new.
#[test]
fn tick_keeps_the_size_limit() {
    let (mut s, _, _, _) = session("budget", 64, 36);
    let dir = s.previews.dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let stale = "f".repeat(32);
    std::fs::write(dir.join(format!("{stale}.mov")), b"x").unwrap();
    s.previews.set_dir(Some(dir.clone()));
    s.previews.cache.set_owner(&owner_id(&s));
    // a fake 2 GB entry: over a 1 GB limit
    s.previews.cache.added(&format!("{stale}.mov"), 2 << 30);
    s.prefs.media_cache.render_cache_gb = 1;
    s.prefs.media_cache.render_cache = "off".into();
    tick(&mut s, true);
    assert!(!dir.join(format!("{stale}.mov")).exists(), "evicted");
    assert_eq!(s.previews.cache.total_bytes(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn index_is_bounded_and_old_previews_are_swept() {
    let (mut s, seq, _, _) = session("index", 64, 36);
    let dir = s.previews.dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    // corrupt and huge indexes are not read (the huge one is sparse: reading it would take long)
    std::fs::write(dir.join(INDEX_FILE), b"{ not json").unwrap();
    assert!(read_index(&dir).is_none());
    let f = std::fs::File::create(dir.join(INDEX_FILE)).unwrap();
    f.set_len(4 << 30).unwrap();
    drop(f);
    let t0 = std::time::Instant::now();
    assert!(read_index(&dir).is_none());
    assert!(t0.elapsed() < Duration::from_secs(1), "not read");
    assert_eq!(read_index_full(&dir), IndexRead::Damaged);
    // a damaged index may be another project's: its folder is never swept
    std::fs::write(dir.join("ffffffffffffffffffffffffffffffff.mov"), b"x").unwrap();
    s.previews.set_dir(Some(dir.clone()));
    assert!(s.previews.cache.legacy().is_empty());
    std::fs::remove_file(dir.join("ffffffffffffffffffffffffffffffff.mov")).unwrap();
    std::fs::remove_file(dir.join(INDEX_FILE)).unwrap();
    // a folder without a usable index: an unused (v1) file is swept, a used one kept
    let used = hashes(&s, seq)[1].clone();
    let v1 = "e".repeat(32);
    for n in [format!("{used}.mov"), format!("{v1}.mov"), format!("{v1}-0-48.mov")] {
        std::fs::write(dir.join(n), b"x").unwrap();
    }
    s.previews.set_dir(Some(dir.clone()));
    assert_eq!(s.previews.cache.legacy().len(), 3);
    s.prefs.media_cache.render_cache = "off".into();
    tick(&mut s, true);
    assert!(dir.join(format!("{used}.mov")).exists());
    assert!(!dir.join(format!("{v1}.mov")).exists() && !dir.join(format!("{v1}-0-48.mov")).exists());
    assert!(s.previews.cache.legacy().is_empty());
    // the index is written and read back; a file in it is never "legacy"
    s.previews.cache.save(true);
    let idx = read_index(&dir).expect("index written");
    assert_eq!(idx.files.keys().cloned().collect::<Vec<_>>(), [format!("{used}.mov")]);
    s.previews.set_dir(None);
    s.previews.set_dir(Some(dir.clone()));
    assert!(s.previews.cache.legacy().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A reader that counts the bytes read, of a huge file whose content `f` makes up.
struct Counting {
    len: u64,
    read: AtomicU64,
    f: fn(u64) -> u8,
}

impl filmcraft_media::ByteReader for Counting {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.len {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
        }
        self.read.fetch_add(buf.len() as u64, Ordering::Relaxed);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (self.f)(offset + i as u64);
        }
        Ok(())
    }
}

/// A 9 GB "QuickTime" file: `ftyp`, an 8 GB `mdat`, then a `moov` claiming 900 MB.
fn huge_mov(off: u64) -> u8 {
    const MDAT: u64 = 8 << 30;
    let mut head = [0u8; 40];
    head[..16].copy_from_slice(&[0, 0, 0, 16, b'f', b't', b'y', b'p', b'q', b't', b' ', b' ', 0, 0, 0, 0]);
    head[16..20].copy_from_slice(&1u32.to_be_bytes());
    head[20..24].copy_from_slice(b"mdat");
    head[24..32].copy_from_slice(&MDAT.to_be_bytes());
    if off < 32 {
        return head[off as usize];
    }
    let moov_at = 16 + MDAT;
    if (moov_at..moov_at + 8).contains(&off) {
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&900_000_000u32.to_be_bytes());
        b[4..].copy_from_slice(b"moov");
        return b[(off - moov_at) as usize];
    }
    0
}

#[test]
fn opening_a_huge_or_corrupt_preview_reads_only_its_head() {
    let s = Session::default();
    for (what, f) in [("damaged moov", huge_mov as fn(u64) -> u8), ("garbage", |o: u64| (o % 251) as u8)] {
        let r = Arc::new(Counting { len: 9 << 30, read: AtomicU64::new(0), f });
        let t0 = std::time::Instant::now();
        assert!(crate::previews::open_part_reader(&s.media, "x.mov", r.clone()).is_none(), "{what}");
        let read = r.read.load(Ordering::Relaxed);
        assert!(read < 1 << 20, "{what}: read {read} bytes");
        assert!(t0.elapsed() < Duration::from_secs(5));
    }
}

/// The cache renders the parts (on the 2 s grid from each segment start), playback reads them,
/// pre-opens the next one, an edit requeues the new hash and undo makes it green again.
#[test]
fn cache_renders_parts_and_playback_uses_them() {
    let (mut s, seq, ids, _) = session("render", 64, 36);
    assert_eq!(s.previews.bar(&s.project, seq).iter().map(|b| b.state).collect::<Vec<_>>(), [BarState::Yellow, BarState::None]);
    settle(&mut s);
    let h = hashes(&s, seq);
    let want: Vec<String> = [&h[0], &h[1]].iter().flat_map(|h| [format!("{h}-0-48.mov"), format!("{h}-48-24.mov")]).collect();
    let mut got = parts(&s);
    got.sort();
    let mut want_sorted = want.clone();
    want_sorted.sort();
    assert_eq!(got, want_sorted, "two parts per yellow segment, nothing for the plain clip");
    let dir = s.previews.dir().unwrap();
    assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part")), "no partial files");
    assert_eq!(s.previews.bar(&s.project, seq).iter().map(|b| b.state).collect::<Vec<_>>(), [BarState::Green, BarState::None]);
    let st = status_json(&s);
    assert_eq!((st["parts"].as_u64(), st["queued"].as_u64(), st["mode"].as_str()), (Some(4), Some(0), Some("smart")), "{st}");
    // playback: frame 50 comes from the second part of the first segment
    assert!(s.previews.frame(&s.media, &s.project, seq, 50, 1.0).is_some());
    assert!(s.previews.is_open(&want[1]));
    // near the end of a part the next one is pre-opened (also across the segment boundary)
    assert_eq!(s.previews.next_part(&s.project, seq, 10), None, "not near the end yet");
    assert_eq!(s.previews.next_part(&s.project, seq, 30).as_deref(), Some(want[1].as_str()));
    assert_eq!(s.previews.next_part(&s.project, seq, 60).as_deref(), Some(want[2].as_str()), "the next segment's first part");
    assert!(s.previews.preopen_next(&s.media, &s.project, seq, 60));
    assert!(s.previews.is_open(&want[2]));
    assert!(!s.previews.preopen_next(&s.media, &s.project, seq, 60), "already open");
    assert_eq!(s.previews.next_part(&s.project, seq, 130), None, "the plain clip has no parts");
    // an edit: the new hash is rendered; undo: the old parts are green again
    s.edit_sequence("Tint", |q, _, _| {
        let (_, it) = q.find_item_mut(ids[1]).unwrap();
        let p = it.effect_mut("tint").unwrap().params.values_mut().find(|p| matches!(p.value, filmcraft_project::ParamValue::Float(_))).unwrap();
        p.value = filmcraft_project::ParamValue::Float(42.0);
        Ok(())
    })
    .unwrap();
    assert_eq!(s.previews.bar(&s.project, seq).iter().map(|b| b.state).collect::<Vec<_>>(), [BarState::Green, BarState::Yellow, BarState::None]);
    settle(&mut s);
    assert_eq!(parts(&s).len(), 6);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.previews.bar(&s.project, seq).iter().map(|b| b.state).collect::<Vec<_>>(), [BarState::Green, BarState::None]);
    // Delete Unused removes the edited hash's parts (no sequence uses it after the undo)
    let v = s.execute("cache.clearUnused", json!({})).unwrap();
    assert_eq!(v["deleted"], 2, "{v}");
    assert_eq!(parts(&s).len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The worker renders nothing while the user plays; the cache queues nothing until idle.
#[test]
fn nothing_renders_while_playing() {
    let (mut s, _, _, _) = session("playing", 64, 36);
    s.background.set_activity(Activity { playing: true, ..Default::default() });
    tick(&mut s, false);
    assert!(s.previews.cache.queued().is_empty(), "busy: nothing queued");
    // queued anyway (forced): the worker waits
    tick(&mut s, true);
    assert!(!s.previews.cache.queued().is_empty());
    std::thread::sleep(Duration::from_millis(400));
    assert!(parts(&s).is_empty(), "nothing rendered while playing");
    assert_eq!(s.background.status().state, crate::background::WorkerState::Waiting);
    assert!(s.previews.cache.building().is_none());
    s.background.set_activity(Activity::default());
    assert!(s.background.wait_idle(Duration::from_secs(60)));
    assert!(!parts(&s).is_empty(), "rendered after playback stopped");
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

#[test]
fn commands_pause_resume_mode_and_cancel_cleans_up() {
    let (mut s, seq, _, _) = session("commands", 64, 36);
    for id in ["cache.setMode", "cache.status", "cache.pause", "cache.resume", "cache.clearUnused"] {
        assert!(crate::commands::find(id).is_some(), "{id}");
    }
    assert!(s.execute("cache.setMode", json!({"mode": "sometimes"})).is_err());
    s.execute("cache.pause", json!({})).unwrap();
    tick(&mut s, true);
    assert!(s.previews.cache.queued().is_empty(), "paused");
    s.execute("cache.setMode", json!({"mode": "off"})).unwrap();
    assert_eq!(s.prefs.media_cache.render_cache, "off");
    s.execute("cache.resume", json!({})).unwrap();
    assert!(s.previews.cache.queued().is_empty(), "off");
    let v = s.execute("cache.setMode", json!({"mode": "all"})).unwrap();
    assert_eq!(v["mode"], "all");
    let _ = s.background.wait_idle(Duration::from_secs(60));
    s.execute("cache.setMode", json!({"mode": "off"})).unwrap();
    // a job cancelled mid-part leaves no partial file and clears the Building state
    let dir = s.previews.dir().unwrap();
    let rate = FrameRate::FPS_24;
    let todo = plan(&s.previews, &s.project, seq, 0, Mode::All, 0).into_iter().next().or_else(|| {
        // everything rendered: delete one and plan again
        s.execute("sequence.deleteRenderFiles", json!({})).unwrap();
        plan(&s.previews, &s.project, seq, 0, Mode::All, 0).into_iter().next()
    });
    let todo = todo.expect("a part to render");
    let name = todo.name();
    let mut job = CacheJob {
        store: s.previews.clone(),
        project: s.project.clone(),
        seq,
        provider: s.media.full_res_provider(s.project.clone(), s.services.clone()),
        dir: dir.clone(),
        name: name.clone(),
        todo: todo.clone(),
        rate,
        format: Format::ProRes,
        exporter: None,
        progress: Progress::default(),
        ended: false,
        id: Arc::new(AtomicU64::new(0)),
    };
    assert_eq!(job.step(), Ok(JobStep::More));
    assert_eq!(s.previews.cache.building(), Some((todo.hash.clone(), todo.off, todo.len)));
    let bar = s.previews.bar(&s.project, seq);
    assert!(bar.iter().any(|b| b.state == BarState::Building), "{bar:?}");
    let _ = job.step();
    std::fs::write(job.partial(), b"partial").unwrap();
    job.cancelled();
    assert!(!job.partial().exists(), "the partial file is deleted");
    assert!(!dir.join(&name).exists());
    assert!(s.previews.cache.building().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

fn job_for(s: &Session, seq: ItemId, format: Format) -> CacheJob {
    let todo = plan(&s.previews, &s.project, seq, 0, Mode::All, 0).into_iter().next().expect("a part");
    CacheJob {
        store: s.previews.clone(),
        project: s.project.clone(),
        seq,
        provider: s.media.full_res_provider(s.project.clone(), s.services.clone()),
        dir: s.previews.dir().unwrap(),
        name: todo.name(),
        todo,
        rate: FrameRate::FPS_24,
        format,
        exporter: None,
        progress: Progress::default(),
        ended: false,
        id: Arc::new(AtomicU64::new(0)),
    }
}

/// Review fix 1: with a size limit smaller than the parts the active sequence wants, the cache
/// stops (status `limitReached`) instead of rendering and deleting the same parts forever.
#[test]
fn a_limit_smaller_than_the_wanted_parts_is_bounded() {
    let (mut s, _, _, _) = session("limit", 64, 36);
    s.previews.cache.set_budget_override(Some(0));
    tick(&mut s, true);
    assert!(s.previews.cache.queued().is_empty(), "nothing fits: nothing queued");
    assert_eq!(status_json(&s)["limitReached"], true);
    // render everything, then shrink the limit below what is rendered: all of it is the active
    // sequence's, so nothing is deleted and nothing is rendered again
    s.previews.cache.set_budget_override(None);
    settle(&mut s);
    let before = parts(&s);
    assert_eq!(before.len(), 4);
    let rendered = s.previews.cache.lock().rendered;
    s.previews.cache.set_budget_override(Some(s.previews.cache.total_bytes() / 2));
    for _ in 0..5 {
        tick(&mut s, true);
        assert!(s.background.wait_idle(Duration::from_secs(60)));
    }
    assert_eq!(parts(&s), before, "the active sequence's parts are kept");
    assert_eq!(s.previews.cache.lock().rendered, rendered, "no part rendered again");
    assert!(s.previews.cache.queued().is_empty());
    // one part missing that doesn't fit: the cache stops, it doesn't delete another part for it
    let gone = before[0].clone();
    s.previews.delete_files(std::slice::from_ref(&gone));
    for _ in 0..5 {
        tick(&mut s, true);
        assert!(s.background.wait_idle(Duration::from_secs(60)));
    }
    let left: Vec<String> = before.iter().filter(|n| **n != gone).cloned().collect();
    assert_eq!(parts(&s), left, "no churn");
    assert_eq!(s.previews.cache.lock().rendered, rendered);
    assert_eq!(status_json(&s)["limitReached"], true);
    // a file of this project no sequence uses goes first when room is needed
    let stale = format!("{}-0-48.mov", "d".repeat(32));
    let dir = s.previews.dir().unwrap();
    std::fs::write(dir.join(&stale), vec![0u8; 4096]).unwrap();
    s.previews.add_video(&stale);
    tick(&mut s, true);
    assert!(!dir.join(&stale).exists(), "an unused file is evicted");
    assert_eq!(parts(&s), left);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review fix 2: a cancelled H.265 part is not an H.265 failure (three of those switch the
/// session to ProRes) and not a failure of the part.
#[test]
fn a_cancelled_hevc_part_is_not_a_failure() {
    let (s, seq, _, _) = session("cancel-hevc", 64, 36);
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
    for _ in 0..MAX_HEVC_FAILURES + 1 {
        let mut job = job_for(&s, seq, Format::Hevc);
        let r = filmcraft_media::cancel::with_cancel(&flag, || job.failed(ExportError::Encode("decode: cancelled".into())));
        assert_eq!(r, Err(CANCELLED.to_string()));
        let mut job = job_for(&s, seq, Format::Hevc);
        assert!(job.failed(ExportError::Cancelled).is_err());
        assert_eq!(job.format, Format::Hevc);
    }
    assert!(s.previews.cache.hevc_ok());
    {
        let c = s.previews.cache.lock();
        assert_eq!((c.hevc_failures, c.failed), (0, 0));
        assert!(c.failures.is_empty());
    }
    // a real encoder failure still counts and falls back to ProRes
    let mut job = job_for(&s, seq, Format::Hevc);
    assert_eq!(job.failed(ExportError::Encode("VA-API: device lost".into())), Ok(JobStep::More));
    assert_eq!(job.format, Format::ProRes);
    assert_eq!(s.previews.cache.lock().hevc_failures, 1);
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

/// Review fix 3: a cancelled job that ends after its part was queued again (a new job) leaves the
/// new job's queue entry and Building state alone; a cancel keeps the entry until the job ends.
#[test]
fn an_old_job_ending_leaves_the_resubmitted_part_alone() {
    let (mut s, seq, _, _) = session("resubmit", 64, 36);
    let name = "a".repeat(32) + "-0-48.mov";
    let running = |job| Running { job, hash: "a".repeat(32), off: 0, len: 48, fraction: 0.0 };
    s.previews.cache.lock().queued.insert(name.clone(), Queued { id: 1 });
    s.previews.cache.set_running(running(1));
    // cancelled, then queued again as job 2 before job 1 ended
    s.previews.cache.lock().queued.insert(name.clone(), Queued { id: 2 });
    s.previews.cache.set_running(running(2));
    s.previews.cache.ended(&name, 1, false, None);
    assert_eq!(s.previews.cache.lock().queued.get(&name).map(|q| q.id), Some(2));
    assert_eq!(s.previews.cache.lock().running.as_ref().map(|r| r.job), Some(2));
    s.previews.cache.set_fraction(1, 0.9);
    assert_eq!(s.previews.cache.lock().running.as_ref().map(|r| r.fraction), Some(0.0), "the old job can't move the new one's progress");
    s.previews.cache.ended(&name, 2, true, None);
    assert!(s.previews.cache.queued().is_empty() && s.previews.cache.building().is_none());
    // the real flow: queued while playing, cancelled, queued again; every part renders once
    s.background.set_activity(Activity { playing: true, ..Default::default() });
    tick(&mut s, true);
    let first = s.previews.cache.queued();
    assert!(!first.is_empty());
    s.execute("cache.pause", json!({})).unwrap();
    assert!(s.previews.cache.queued().len() <= first.len());
    s.execute("cache.resume", json!({})).unwrap();
    s.background.set_activity(Activity::default());
    settle(&mut s);
    assert!(s.previews.cache.queued().is_empty() && s.previews.cache.building().is_none());
    assert_eq!(parts(&s).len(), 4);
    assert!(plan(&s.previews, &s.project, seq, 0, Mode::Smart, 0).is_empty());
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

/// Review fixes 4 + 5: sizes come from the files, never from `index.json`; names in the index
/// that are not files of the folder are ignored; crash leftovers (`*.mov.part`,
/// `index.json.part`) are deleted on load, nothing else.
#[test]
fn the_index_is_hostile_and_leftovers_are_swept() {
    let dir = tmp("hostile");
    std::fs::create_dir_all(&dir).unwrap();
    let h = "b".repeat(32);
    let real = format!("{h}-0-48.mov");
    std::fs::write(dir.join(&real), b"abc").unwrap();
    let outside = dir.parent().unwrap().join(format!("filmcraft-outside-{}.mov", std::process::id()));
    std::fs::write(&outside, b"keep").unwrap();
    let idx = json!({
        "version": HASH_VERSION,
        "files": {
            real.clone(): {"bytes": 9_000_000_000_000_000_000u64, "lastUsed": 5, "project": "/p/Proj.fcp"},
            "../x.mov": {"bytes": 1, "lastUsed": 1},
            outside.to_string_lossy(): {"bytes": 1, "lastUsed": 1, "project": ""},
        }
    });
    std::fs::write(dir.join(INDEX_FILE), serde_json::to_vec(&idx).unwrap()).unwrap();
    // leftovers: old ones of the cache's own patterns go, a fresh one and other files stay
    let old = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
    for n in [format!("{h}-48-48.mov.part"), format!("{INDEX_FILE}.part"), "notes.txt.part".into(), "other.part".into(), format!("{h}.mov.part.bak")] {
        let f = std::fs::File::create(dir.join(&n)).unwrap();
        f.set_modified(old).unwrap();
    }
    std::fs::write(dir.join(format!("{h}-96-48.mov.part")), b"rendering").unwrap();
    let mut s = Session::default();
    s.previews.set_dir(Some(dir.clone()));
    let index = s.previews.cache.index();
    assert_eq!(index.keys().cloned().collect::<Vec<_>>(), std::slice::from_ref(&real), "only files of the folder");
    assert_eq!(index[&real].bytes, 3, "the size from the file system");
    assert_eq!(index[&real].last_used, 5);
    assert_eq!(s.previews.cache.total_bytes(), 3);
    assert!(!dir.join(format!("{h}-48-48.mov.part")).exists() && !dir.join(format!("{INDEX_FILE}.part")).exists());
    for n in ["notes.txt.part".to_string(), "other.part".into(), format!("{h}.mov.part.bak"), format!("{h}-96-48.mov.part")] {
        assert!(dir.join(&n).exists(), "{n} kept");
    }
    // the size limit and Delete Unused never reach outside the folder
    s.previews.cache.set_budget_override(Some(0));
    s.prefs.media_cache.render_cache = "off".into();
    tick(&mut s, true);
    s.execute("cache.clearUnused", json!({})).unwrap();
    assert!(outside.exists());
    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review fix 6: two projects with the same name share a preview folder (one scratch disk): the
/// size limit and Delete Unused only delete this project's files.
#[test]
fn a_shared_folder_keeps_the_other_projects_files() {
    let (mut s, _, _, _) = session("shared", 64, 36);
    let dir = s.previews.dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let name = |c: char| format!("{}-0-48.mov", c.to_string().repeat(32));
    let (theirs, unknown, mine) = (name('a'), name('c'), name('e'));
    for n in [&theirs, &unknown, &mine] {
        std::fs::write(dir.join(n), vec![0u8; 1000]).unwrap();
    }
    let idx = json!({
        "version": HASH_VERSION,
        "files": {
            theirs.clone(): {"lastUsed": 1, "project": "/other/Proj.fcp"},
            mine.clone(): {"lastUsed": 1, "project": "/mine/Proj.fcp"},
        }
    });
    std::fs::write(dir.join(INDEX_FILE), serde_json::to_vec(&idx).unwrap()).unwrap();
    s.path = Some("/mine/Proj.fcp".into());
    s.previews.set_dir(Some(dir.clone()));
    let v = s.execute("cache.clearUnused", json!({})).unwrap();
    assert_eq!(v["deleted"], 1, "{v}");
    assert!(!dir.join(&mine).exists());
    assert!(dir.join(&theirs).exists() && dir.join(&unknown).exists());
    // the size limit: far over it, still only this project's files
    s.previews.cache.set_budget_override(Some(0));
    s.prefs.media_cache.render_cache = "off".into();
    tick(&mut s, true);
    assert!(dir.join(&theirs).exists() && dir.join(&unknown).exists());
    assert_eq!(status_json(&s)["otherFiles"], 2);
    // the index keeps whose files are whose
    s.previews.cache.save(true);
    let idx = read_index(&dir).unwrap();
    assert_eq!(idx.files[&theirs].project.as_deref(), Some("/other/Proj.fcp"));
    assert_eq!(idx.files[&unknown].project, None);
    // an unsaved project's files become the project's own when it is saved
    s.path = None;
    s.previews.cache.set_owner("");
    s.previews.cache.added(&mine, 10);
    s.path = Some("/mine/Proj.fcp".into());
    s.previews.cache.set_owner("/mine/Proj.fcp");
    assert_eq!(s.previews.cache.index()[&mine].project.as_deref(), Some("/mine/Proj.fcp"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review fix 7: a source file re-exported in place (same path, other size / time) is another
/// picture: every hash of its clips changes.
#[test]
fn a_source_rewritten_in_place_changes_the_hash() {
    let (mut s, seq, _, matte) = session("rewrite", 64, 36);
    let file = tmp("rewrite-src").with_extension("mov");
    std::fs::write(&file, b"first").unwrap();
    let path = file.to_string_lossy().to_string();
    edit_project(&mut s, |p| {
        if let Some(ItemKind::Media(m)) = p.items.get_mut(&matte).map(|i| &mut i.kind) {
            m.media = MediaRef::File { path: path.clone() };
        }
    });
    let h0 = hashes(&s, seq);
    edit_project(&mut s, |_| {});
    assert_eq!(hashes(&s, seq), h0, "unchanged file: same hashes");
    std::fs::write(&file, b"second, longer").unwrap();
    edit_project(&mut s, |_| {});
    let h1 = hashes(&s, seq);
    assert!(h1.iter().zip(&h0).all(|(a, b)| a != b), "rewritten: {h0:?} {h1:?}");
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

/// Phase 6 owed: `cache.claimFolder` makes files of no known project this project's, so Delete
/// Unused can clean them; another project's files stay theirs; no folder is refused.
#[test]
fn claim_folder_takes_only_files_of_no_known_project() {
    let (mut s, _, _, _) = session("claim", 64, 36);
    let dir = s.previews.dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let name = |c: char| format!("{}-0-48.mov", c.to_string().repeat(32));
    let (theirs, unknown) = (name('a'), name('c'));
    for n in [&theirs, &unknown] {
        std::fs::write(dir.join(n), vec![0u8; 1000]).unwrap();
    }
    let idx = json!({"version": HASH_VERSION, "files": {theirs.clone(): {"lastUsed": 1, "project": "/other/Proj.fcp"}}});
    std::fs::write(dir.join(INDEX_FILE), serde_json::to_vec(&idx).unwrap()).unwrap();
    s.path = Some("/mine/Proj.fcp".into());
    s.previews.set_dir(Some(dir.clone()));
    assert_eq!(status_json(&s)["unknownFiles"], 1);
    let v = s.execute("cache.claimFolder", json!({})).unwrap();
    assert_eq!((v["claimed"].as_u64(), v["bytes"].as_u64()), (Some(1), Some(1000)), "{v}");
    assert_eq!(status_json(&s)["unknownFiles"], 0);
    assert_eq!(read_index(&dir).unwrap().files[&unknown].project.as_deref(), Some("/mine/Proj.fcp"));
    // claiming again changes nothing; Delete Unused now cleans the claimed file, never theirs
    assert_eq!(s.execute("cache.claimFolder", json!({})).unwrap()["claimed"], 0);
    assert_eq!(s.execute("cache.clearUnused", json!({})).unwrap()["deleted"], 1);
    assert!(!dir.join(&unknown).exists() && dir.join(&theirs).exists());
    let _ = std::fs::remove_dir_all(&dir);
    // no folder: refused
    s.previews.set_dir(None);
    assert!(s.execute("cache.claimFolder", json!({})).is_err());
}
