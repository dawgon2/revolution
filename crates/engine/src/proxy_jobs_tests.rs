//! Automatic proxies (fork 6.2): which clips get them, the background job (attach, fallback,
//! cancel), identity checks, proxy modes and the commands.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use filmcraft_media::{DemoScene, MediaInfo, MediaKind, VideoStreamInfo};
use filmcraft_project::{ItemId, MediaRef};
use serde_json::json;

use crate::Session;
use crate::autosave::MediaPrefs;
use crate::background::BackgroundJob;
use crate::media_pool::{ProxyMode, with_motion};
use crate::media_test_util::{make_movie, session_with, tmp_dir};
use crate::proxies::{HEVC_HALF, preset};
use crate::proxy_jobs::{self, ProxyJob, ProxyState, Reason, auto_reason, proxy_file_name, proxy_fingerprint, software_422, software_h264_high_bit_depth};

const W: u32 = 160;
const H: u32 = 96;

fn info(codec: &str, pixfmt: &str, w: u32, h: u32) -> MediaInfo {
    MediaInfo {
        name: "x".into(),
        kind: MediaKind::Movie,
        duration: filmcraft_time::Tick(filmcraft_time::TICKS_PER_SECOND),
        video: Some(VideoStreamInfo {
            camera_color_space: None,
            width: w,
            height: h,
            frame_rate: filmcraft_time::FrameRate::FPS_24,
            par: (1, 1),
            codec: codec.into(),
            pixel_format: pixfmt.into(),
            color: Default::default(),
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: vec![],
        container: "mov".into(),
        start_timecode: None,
        file_size: None,
    }
}

/// Make the item look like `codec` / `pixfmt` (and optionally `w`×`h`) footage; the file stays the
/// small ProRes movie, so a job still has real pictures to transcode.
fn pretend(s: &mut Session, item: ItemId, codec: &str, pixfmt: &str, size: Option<(u32, u32)>) {
    let mut p = (*s.project).clone();
    let m = p.item_mut(item).unwrap().as_media_mut().unwrap();
    let v = m.info.video.as_mut().unwrap();
    v.codec = codec.into();
    v.pixel_format = pixfmt.into();
    if let Some((w, h)) = size {
        (v.width, v.height) = (w, h);
    }
    s.project = Arc::new(p);
}

fn status(s: &mut Session) -> serde_json::Value {
    s.execute("media.proxyStatus", json!({})).unwrap()
}

fn proxy_of(s: &Session, item: ItemId) -> Option<String> {
    match &s.project.item(item)?.as_media()?.proxy {
        Some(MediaRef::File { path }) => Some(path.clone()),
        _ => None,
    }
}

/// A session over one small movie with the ProRes proxy preset first (no VA-API in tests).
fn setup(name: &str, frames: i64) -> (Session, ItemId, std::path::PathBuf) {
    let root = tmp_dir(name);
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::OceanSunset, W, H, frames);
    let (mut s, items, _) = session_with(&[&a]);
    s.auto_proxies.first_preset = preset("prores_proxy_half");
    s.auto_proxies.enabled = true;
    (s, items[0], root)
}

fn settle(s: &mut Session) {
    for _ in 0..4 {
        assert!(s.background.wait_idle(Duration::from_secs(60)), "the worker finishes");
        s.poll_persistence();
        if s.auto_proxies.pending() == 0 {
            return;
        }
    }
}

#[test]
fn which_clips_get_automatic_proxies() {
    let mut prefs = MediaPrefs::default();
    assert_eq!((prefs.proxies, prefs.auto_proxies), (ProxyMode::Auto, true), "defaults: auto, automatic proxies on");
    let hevc422 = info("HEVC", "YUV 4:2:2 10-bit", 1920, 1080);
    let avc422 = info("H.264 (High 4:2:2)", "YUV 4:2:2 10-bit", 1920, 1080);
    let prores = info("ProRes 422 HQ", "YUV 4:2:2 10-bit", 6240, 4160);
    let hevc62k = info("HEVC", "YUV 4:2:0 10-bit", 6240, 4160);
    let uhd = info("H.264 (High)", "YUV 4:2:0 8-bit", 3840, 2160);
    let hd = info("HEVC", "YUV 4:2:0 10-bit", 1920, 1080);
    assert_eq!(auto_reason(&hevc422, &prefs), Some(Reason::Chroma422));
    assert_eq!(auto_reason(&avc422, &prefs), Some(Reason::Chroma422));
    assert_eq!(auto_reason(&prores, &prefs), None, "intra codecs decode fast enough");
    assert_eq!(auto_reason(&hevc62k, &prefs), Some(Reason::Large));
    assert_eq!(auto_reason(&uhd, &prefs), Some(Reason::Large));
    assert_eq!(auto_reason(&hd, &prefs), None);
    let mut still = hevc62k.clone();
    still.kind = MediaKind::Still;
    assert_eq!(auto_reason(&still, &prefs), None);
    prefs.auto_proxies = false;
    assert_eq!(auto_reason(&hevc62k, &prefs), None, "large clips follow the preference");
    assert_eq!(auto_reason(&hevc422, &prefs), Some(Reason::Chroma422), "4:2:2 always");
    prefs.proxies = ProxyMode::Off;
    assert_eq!(auto_reason(&hevc422, &prefs), None, "nothing while proxies are off");
    assert!(software_422(&hevc422));
    assert!(software_422(&avc422), "H.264 High 4:2:2 has no hardware decoder anywhere (H3.2)");
    assert!(!software_422(&prores) && !software_422(&hevc62k) && !software_422(&uhd));
    assert!(!proxy_jobs::hardware_422_failed(&avc422), "no H.264 4:2:2 hardware path to fail");
}

#[test]
fn h264_high_bit_depth_clips_say_software_decode() {
    let high10 = info("H.264 (High 10)", "YUV 4:2:0 10-bit", 1920, 1080);
    let high = info("H.264 (High)", "YUV 4:2:0 8-bit", 1920, 1080);
    let avc422 = info("H.264 (High 4:2:2)", "YUV 4:2:2 10-bit", 1920, 1080);
    let hevc10 = info("HEVC", "YUV 4:2:0 10-bit", 1920, 1080);
    assert!(software_h264_high_bit_depth(&high10));
    assert!(!software_h264_high_bit_depth(&high), "8-bit H.264 decodes in hardware");
    assert!(!software_h264_high_bit_depth(&avc422), "4:2:2 gets the 4:2:2 badge instead");
    assert!(!software_h264_high_bit_depth(&hevc10), "10-bit HEVC decodes in hardware");
    // hostile / odd labels: no panic, no badge
    for label in ["", "-bit", "YUV 4:2:0 99999999999-bit", "10", "YUV 4:2:0 -bit 10"] {
        let mut v = high10.clone();
        if let Some(s) = v.video.as_mut() {
            s.pixel_format = label.into();
        }
        assert!(!software_h264_high_bit_depth(&v), "{label:?}");
    }
    let bits = |p: &str| high10.video.as_ref().map(|v| VideoStreamInfo { pixel_format: p.into(), ..v.clone() }).and_then(|v| proxy_jobs::bit_depth(&v));
    assert_eq!(bits("YUV 4:2:0 10-bit"), Some(10));
    assert_eq!(bits("YUV 4:2:2 8-bit"), Some(8));
    assert_eq!(bits("YUV 4:2:0 ✓10-bit"), Some(10), "multi-byte characters before the digits");
    assert_eq!(bits("RGB"), None);
}

#[test]
fn proxy_names_carry_the_fingerprint() {
    let n = proxy_file_name("A001", 0x0123_4567_89ab_cdef, "mov");
    assert_eq!(n, "A001_Proxy.0123456789abcdef.mov");
    assert_eq!(proxy_fingerprint(&format!("/x/Proxies/{n}")), Some(0x0123_4567_89ab_cdef));
    assert_eq!(proxy_fingerprint("/x/Proxies/A001_Proxy.mov"), None, "a hand-made proxy");
    assert_eq!(proxy_fingerprint("/x/Proxies/A001_Proxy.0123456789abcdef.partial.mov"), None);
    assert_eq!(proxy_fingerprint("/x/A001.0123456789abcdef.mov"), None);
    assert_eq!(proxy_fingerprint("/x/A001_Proxy.zz23456789abcdef.mov"), None);
}

#[test]
fn a_422_import_gets_a_proxy_automatically() {
    let (mut s, item, root) = setup("autoproxy-422", 12);
    assert_eq!(status(&mut s)["queued"], json!(0), "a ProRes clip gets none");
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    proxy_jobs::on_import(&mut s, &[item]);
    let st = status(&mut s);
    assert_eq!(st["queued"].as_u64().unwrap() + st["building"].as_u64().unwrap(), 1, "{st}");
    assert_eq!(st["items"][0]["reason"], json!("chroma422"));
    assert!(s.auto_proxies.state(item).is_some());
    settle(&mut s);
    let p = proxy_of(&s, item).expect("attached when done");
    let fp = s.project.item(item).unwrap().as_media().unwrap().identity.unwrap().fingerprint;
    assert!(Path::new(&p).ends_with(Path::new("Proxies").join(proxy_file_name("a", fp, "mov"))), "{p}");
    assert!(Path::new(&p).is_file());
    assert!(!Path::new(&p.replace(".mov", ".partial.mov")).exists(), "no partial file left");
    assert_eq!(s.auto_proxies.state(item), None, "ready");
    let st = status(&mut s);
    assert_eq!((st["ready"].as_u64(), st["failed"].as_u64()), (Some(1), Some(0)), "{st}");
    // the proxy is half size
    let src = s.media.open_file(&p, &*s.services).unwrap();
    assert_eq!(src.info().video.as_ref().map(|v| (v.width, v.height)), Some((W / 2, H / 2)));
    // a second import of the same media finds the finished proxy instead of encoding again
    let (mut t, items, _) = session_with(&[&root.join("a.mov")]);
    t.auto_proxies.first_preset = preset("prores_proxy_half");
    t.auto_proxies.enabled = true;
    pretend(&mut t, items[0], "HEVC", "YUV 4:2:2 10-bit", None);
    proxy_jobs::on_import(&mut t, &[items[0]]);
    assert_eq!(t.auto_proxies.pending(), 0);
    assert_eq!(proxy_of(&t, items[0]), Some(p));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn large_long_gop_clips_follow_the_preference() {
    let (mut s, item, root) = setup("autoproxy-large", 6);
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:0 10-bit", Some((6240, 4160)));
    s.prefs.media.auto_proxies = false;
    proxy_jobs::on_import(&mut s, &[item]);
    assert_eq!(s.auto_proxies.pending(), 0, "off: nothing queued");
    // switching it on queues the project's large clips
    s.execute("media.autoProxies", json!({"enabled": true})).unwrap();
    assert_eq!(s.auto_proxies.pending(), 1);
    assert_eq!(s.auto_proxies.state(item), Some(ProxyState::Queued));
    assert_eq!(s.execute("media.cancelProxies", json!({})).unwrap()["cancelled"], json!(1));
    proxy_jobs::on_import(&mut s, &[item]);
    assert_eq!(s.auto_proxies.pending(), 1, "on: queued on import");
    s.execute("media.cancelProxies", json!({"items": [item.0]})).unwrap();
    s.background.resume();
    assert!(s.background.wait_idle(Duration::from_secs(30)));
    assert_eq!(proxy_of(&s, item), None, "cancelled before it ran");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_proxy_of_another_file_is_detached_and_made_again() {
    let (mut s, item, root) = setup("autoproxy-identity", 12);
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    // an automatic proxy made from another file (its name carries another fingerprint)
    let dir = root.join("Proxies");
    std::fs::create_dir_all(&dir).unwrap();
    let wrong = dir.join(proxy_file_name("a", 1, "mov"));
    make_movie(&wrong, DemoScene::Aurora, W / 2, H / 2, 12);
    s.execute("media.attachProxies", json!({"item": item.0, "path": wrong.to_string_lossy()})).unwrap();
    proxy_jobs::after_relink(&mut s, &[item]);
    assert_eq!(proxy_of(&s, item), None, "detached");
    assert_eq!(s.auto_proxies.pending(), 1, "a new one is queued");
    // a hand-made proxy is left alone
    proxy_jobs::cancel(&mut s, None);
    let hand = dir.join("a_Proxy.mov");
    make_movie(&hand, DemoScene::Aurora, W / 2, H / 2, 12);
    s.execute("media.attachProxies", json!({"item": item.0, "path": hand.to_string_lossy()})).unwrap();
    proxy_jobs::after_relink(&mut s, &[item]);
    assert_eq!(proxy_of(&s, item), Some(hand.to_string_lossy().into_owned()));
    assert_eq!(s.auto_proxies.pending(), 0);
    s.background.resume();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn opening_a_project_checks_and_queues_proxies() {
    let (mut s, item, root) = setup("autoproxy-open", 12);
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    let proj = root.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.saveAs", json!({"path": proj})).unwrap();
    s.background.pause();
    s.execute("file.open", json!({"path": proj})).unwrap();
    assert_eq!(s.auto_proxies.pending(), 1, "the 4:2:2 clip gets a proxy on open");
    s.background.resume();
    settle(&mut s);
    assert!(proxy_of(&s, item).is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cancel_deletes_the_partial_file() {
    let (s, item, root) = setup("autoproxy-cancel", 48);
    let out = root.join("Proxies").join(proxy_file_name("a", 7, "mov"));
    let mut job = ProxyJob::for_test(&s, item, &out, preset("prores_proxy_half").unwrap());
    let partial = job.partial().to_path_buf();
    for _ in 0..200 {
        if partial.is_file() {
            break;
        }
        assert_eq!(job.step(), Ok(crate::background::JobStep::More), "still running");
    }
    assert!(partial.is_file(), "the job writes its partial file");
    job.cancelled();
    assert!(!partial.exists(), "cancel deletes the partial file");
    assert!(!out.exists());
    assert_eq!(job.outcome(), "Cancelled");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_hevc_proxy_falls_back_to_prores() {
    let (mut s, item, root) = setup("autoproxy-fallback", 12);
    s.auto_proxies.first_preset = Some(&HEVC_HALF);
    s.auto_proxies.fail_hevc = true;
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    proxy_jobs::on_import(&mut s, &[item]);
    assert_eq!(status(&mut s)["items"][0]["preset"], json!("hevc_half"));
    assert!(s.background.wait_idle(Duration::from_secs(30)));
    s.poll_persistence();
    let st = status(&mut s);
    assert_eq!(st["items"][0]["preset"], json!("prores_proxy_half"), "requeued once with the fallback: {st}");
    settle(&mut s);
    let p = proxy_of(&s, item).expect("the fallback proxy is attached");
    assert!(p.ends_with(".mov"));
    assert_eq!(status(&mut s)["failed"], json!(0));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_proxy_that_fails_twice_is_reported() {
    let (mut s, item, root) = setup("autoproxy-fail", 12);
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    s.background.pause();
    proxy_jobs::on_import(&mut s, &[item]);
    // the original disappears before the job runs
    std::fs::remove_file(root.join("a.mov")).unwrap();
    s.background.resume();
    settle(&mut s);
    assert!(matches!(s.auto_proxies.state(item), Some(ProxyState::Failed { .. })), "{:?}", s.auto_proxies.state(item));
    assert_eq!(status(&mut s)["failed"], json!(1));
    assert!(!root.join("Proxies").read_dir().map(|d| d.count() > 0).unwrap_or(false), "no files left behind");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn auto_mode_uses_proxies_only_in_motion_and_export_never() {
    let root = tmp_dir("autoproxy-mode");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let other = root.join("other.mov");
    make_movie(&other, DemoScene::Plasma, W / 2, H / 2, 12);
    let (mut s, items, _) = session_with(&[&a]);
    let item = items[0];
    s.execute("media.attachProxies", json!({"item": item.0, "path": other.to_string_lossy()})).unwrap();
    assert_eq!(s.media.proxy_mode(), ProxyMode::Auto);
    let full = s.media.full_res_source(&s.project, item, &*s.services).unwrap();
    let paused = s.media.source_for(&s.project, item, &*s.services).unwrap();
    assert!(Arc::ptr_eq(&paused, &full), "a paused frame reads the original");
    let playing = with_motion(true, || s.media.source_for(&s.project, item, &*s.services).unwrap());
    assert!(!Arc::ptr_eq(&playing, &full), "playback reads the proxy");
    assert!(Arc::ptr_eq(&with_motion(false, || s.media.source_for(&s.project, item, &*s.services).unwrap()), &full));
    // without a hint, Auto follows what the user does (the background worker's activity)
    use crate::background::Activity;
    s.background.set_activity(Activity { playing: true, ..Default::default() });
    s.poll_persistence();
    assert!(!Arc::ptr_eq(&s.media.source_for(&s.project, item, &*s.services).unwrap(), &full), "playing: the proxy");
    let rev = s.revision;
    let dirty = s.is_dirty();
    s.background.set_activity(Activity::default());
    s.poll_persistence();
    assert!(Arc::ptr_eq(&s.media.source_for(&s.project, item, &*s.services).unwrap(), &full), "stopped: the original");
    assert!(s.revision > rev, "the paused frame is redrawn");
    assert_eq!(s.is_dirty(), dirty, "not an edit");
    // export reads the originals even from inside playback's hint
    let p = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let exported = with_motion(true, || filmcraft_render::SourceProvider::source(&p, item).unwrap());
    assert!(Arc::ptr_eq(&exported, &full));
    let png = root.join("out.png").to_string_lossy().into_owned();
    with_motion(true, || s.execute("file.exportMedia", json!({"path": png, "format": "png", "wait": true}))).unwrap();
    let f = image::open(root.join("out004.png")).unwrap().to_rgba8();
    let full_px = crate::media_test_util::frame_rgba(&mut s, 4, 1.0);
    assert_eq!(f.as_raw(), &full_px.2, "export renders full-resolution media");
    // On: every frame; Off: none
    s.execute("media.proxyMode", json!({"mode": "on"})).unwrap();
    assert!(!Arc::ptr_eq(&s.media.source_for(&s.project, item, &*s.services).unwrap(), &full));
    s.execute("media.proxyMode", json!({"mode": "off"})).unwrap();
    assert!(Arc::ptr_eq(&with_motion(true, || s.media.source_for(&s.project, item, &*s.services).unwrap()), &full));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn proxy_commands_round_trip() {
    let mut s = Session::default();
    assert_eq!(s.execute("media.proxyMode", json!({})).unwrap()["mode"], json!("auto"));
    for m in ["on", "off", "auto"] {
        assert_eq!(s.execute("media.proxyMode", json!({"mode": m})).unwrap()["mode"], json!(m));
        assert_eq!(s.prefs.media.proxies, ProxyMode::parse(m).unwrap());
        assert_eq!(s.media.proxy_mode(), s.prefs.media.proxies, "the media pool follows");
    }
    assert!(s.execute("media.proxyMode", json!({"mode": "sometimes"})).is_err());
    assert_eq!(s.execute("media.autoProxies", json!({"enabled": false})).unwrap()["enabled"], json!(false));
    assert!(!s.prefs.media.auto_proxies);
    assert_eq!(s.execute("media.autoProxies", json!({})).unwrap()["enabled"], json!(true), "toggles");
    // Toggle Proxies: on / off (Auto lives in Settings ▸ Media ▸ Proxies; from Auto it turns on)
    assert_eq!(s.execute("media.toggleProxies", json!({})).unwrap()["mode"], json!("on"));
    assert!(s.media.use_proxies());
    assert_eq!(s.execute("media.toggleProxies", json!({})).unwrap()["mode"], json!("off"));
    assert_eq!(s.execute("media.toggleProxies", json!({})).unwrap()["mode"], json!("on"));
    assert_eq!(s.execute("media.toggleProxies", json!({"enabled": false})).unwrap()["mode"], json!("off"));
    let st = s.execute("media.proxyStatus", json!({})).unwrap();
    for k in ["mode", "autoProxies", "hevcEncode", "queued", "building", "failed", "ready", "progress", "worker", "items"] {
        assert!(st.get(k).is_some(), "{k} in {st}");
    }
    assert_eq!(s.execute("media.cancelProxies", json!({})).unwrap()["cancelled"], json!(0));
    // Settings ▸ Media rows
    assert_eq!(s.prefs.get("media.proxies"), Some(json!("off")));
    assert_eq!(s.prefs.get("media.autoProxies"), Some(json!(true)));
    assert!(crate::settings::validate("media.proxies", &json!("auto")).is_ok());
    assert!(crate::settings::validate("media.proxies", &json!("maybe")).is_err());
}

#[test]
fn old_enable_proxies_preference_reads_as_a_mode() {
    use crate::autosave::Preferences;
    let p = Preferences::from_value(json!({"version": 2, "media": {"enableProxies": false}}));
    assert_eq!(p.media.proxies, ProxyMode::Auto, "off was only off because there were none");
    let p = Preferences::from_value(json!({"version": 2, "media": {"enableProxies": true}}));
    assert_eq!(p.media.proxies, ProxyMode::On);
    let p = Preferences::from_value(json!({"version": 2, "media": {"enableProxies": true, "proxies": "off"}}));
    assert_eq!(p.media.proxies, ProxyMode::Off, "the new key wins");
    let p = Preferences::from_value(json!({"version": 2, "media": {"enableProxies": false, "proxies": "on"}}));
    assert_eq!(p.media.proxies, ProxyMode::On, "the new key wins over an old off too");
    assert!(p.to_value()["media"].get("enableProxies").is_none());
    let p = Preferences::from_value(json!({"version": 2, "media": {"proxies": "nonsense"}}));
    assert_eq!(p.media.proxies, ProxyMode::Auto, "repaired to the default");
    let v = p.to_value();
    assert!(v["media"].get("enableProxies").is_none(), "{}", v["media"]);
    assert_eq!(v["media"]["proxies"], json!("auto"));
}

fn fingerprint(s: &Session, item: ItemId) -> u64 {
    s.project.item(item).unwrap().as_media().unwrap().identity.unwrap().fingerprint
}

/// A CLI / headless session (the flag the desktop app sets is off) queues nothing on its own.
#[test]
fn headless_sessions_queue_nothing() {
    let (mut s, item, root) = setup("autoproxy-headless", 6);
    s.auto_proxies.enabled = false;
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    proxy_jobs::on_import(&mut s, &[item]);
    assert_eq!(s.auto_proxies.pending(), 0, "import");
    let proj = root.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.saveAs", json!({"path": proj})).unwrap();
    s.execute("file.open", json!({"path": proj})).unwrap();
    assert_eq!(s.auto_proxies.pending(), 0, "open");
    s.execute("media.proxyMode", json!({"mode": "off"})).unwrap();
    s.execute("media.proxyMode", json!({"mode": "auto"})).unwrap();
    assert_eq!(s.auto_proxies.pending(), 0, "preferences widening");
    assert!(!root.join("Proxies").exists(), "nothing written");
    // asked for explicitly, it still works
    let r = s.execute("media.createProxies", json!({"item": item.0, "preset": "auto"})).unwrap();
    assert_eq!(r["items"][0]["queued"]["result"], json!("job"), "{r}");
    proxy_jobs::cancel(&mut s, None);
    s.background.resume();
    let _ = std::fs::remove_dir_all(&root);
}

/// A proxy finishing in the background is not an undo step: the redo stack survives, undo undoes
/// the user's own edit, and neither undo nor redo loses the proxy.
#[test]
fn a_finished_proxy_stays_out_of_the_undo_history() {
    let (mut s, item, root) = setup("autoproxy-undo", 12);
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    proxy_jobs::on_import(&mut s, &[item]);
    assert_eq!(s.auto_proxies.pending(), 1);
    let rename = |s: &mut Session, n: &str| {
        let n = n.to_string();
        s.edit("Rename", move |p, _| {
            p.item_mut(item).unwrap().name = n;
            Ok(())
        })
        .unwrap();
    };
    rename(&mut s, "first");
    rename(&mut s, "second");
    assert!(s.undo().is_some());
    assert_eq!(s.project.item(item).unwrap().name, "first");
    let (undo_n, redo_n) = (s.history.undo.len(), s.history.redo.len());
    assert_eq!(redo_n, 1);
    s.background.resume();
    settle(&mut s);
    let proxy = proxy_of(&s, item).expect("attached");
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo_n, redo_n), "no undo step, redo kept");
    assert!(s.redo().is_some(), "redo still works");
    assert_eq!(s.project.item(item).unwrap().name, "second");
    assert_eq!(proxy_of(&s, item).as_ref(), Some(&proxy), "redo keeps the proxy");
    assert!(s.undo().is_some());
    assert!(s.undo().is_some(), "Ctrl+Z undoes the user's own edits");
    assert_ne!(s.project.item(item).unwrap().name, "first");
    assert_eq!(proxy_of(&s, item).as_ref(), Some(&proxy), "undo keeps the proxy");
    let _ = std::fs::remove_dir_all(&root);
}

/// Two items on the same file share one job (one output file) and both get the proxy.
#[test]
fn items_on_the_same_file_share_one_proxy_job() {
    let (mut s, item, root) = setup("autoproxy-dup", 12);
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    s.execute("project.select", json!({"items": [item.0]})).unwrap();
    let dup = ItemId(s.execute("edit.duplicate", json!({})).unwrap()["items"][0].as_u64().unwrap());
    proxy_jobs::on_import(&mut s, &[item, dup]);
    assert_eq!(s.auto_proxies.pending(), 1, "one job");
    assert_eq!(s.background.status().jobs.len(), 1);
    assert!(s.auto_proxies.state(dup).is_some(), "the duplicate waits for it");
    s.background.resume();
    settle(&mut s);
    let p = proxy_of(&s, item).expect("attached");
    assert_eq!(proxy_of(&s, dup).as_ref(), Some(&p), "attached to both");
    assert!(Path::new(&p).is_file(), "the first item's proxy was not deleted");
    let _ = std::fs::remove_dir_all(&root);
}

/// A file a project item uses as its media is never deleted or overwritten, even where an
/// automatic proxy would go.
#[test]
fn a_clip_in_the_project_is_never_deleted() {
    let (mut s, item, root) = setup("autoproxy-safe", 12);
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    let out = root.join("Proxies").join(proxy_file_name("a", fingerprint(&s, item), "mov"));
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    // does not fit the clip (another length), and the user imported it as a clip
    make_movie(&out, DemoScene::Aurora, W / 2, H / 2, 3);
    s.execute("file.import", json!({"paths": [out.to_string_lossy()]})).unwrap();
    assert!(proxy_jobs::used_as_original(&s.project, &out));
    proxy_jobs::on_import(&mut s, &[item]);
    assert_eq!(s.auto_proxies.pending(), 0);
    assert!(s.execute("media.createProxies", json!({"item": item.0, "preset": "auto"})).is_err());
    assert!(out.is_file(), "kept");
    s.background.resume();
    let _ = std::fs::remove_dir_all(&root);
}

/// A misfitting proxy file nobody uses is replaced; a file that appears at the output while the
/// job runs is not overwritten.
#[test]
fn stale_proxies_are_replaced_but_new_files_are_not_overwritten() {
    let (mut s, item, root) = setup("autoproxy-stale", 12);
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    let out = root.join("Proxies").join(proxy_file_name("a", fingerprint(&s, item), "mov"));
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    make_movie(&out, DemoScene::Aurora, W / 2, H / 2, 3);
    proxy_jobs::on_import(&mut s, &[item]);
    assert_eq!(s.auto_proxies.pending(), 1, "requeued");
    assert!(!out.exists(), "the stale proxy was removed");
    proxy_jobs::cancel(&mut s, None);
    s.background.resume();
    // a job whose output appears meanwhile fails instead of replacing it
    let other = root.join("Proxies").join("x_Proxy.0000000000000001.mov");
    let mut job = ProxyJob::for_test(&s, item, &other, preset("prores_proxy_half").unwrap());
    make_movie(&other, DemoScene::Plasma, W / 2, H / 2, 3);
    let before = std::fs::metadata(&other).unwrap().len();
    let mut r = Ok(crate::background::JobStep::More);
    for _ in 0..500 {
        r = job.step();
        if r != Ok(crate::background::JobStep::More) {
            break;
        }
    }
    assert!(matches!(&r, Err(e) if e.contains("not replaced")), "{r:?}");
    assert_eq!(std::fs::metadata(&other).unwrap().len(), before, "untouched");
    assert!(!job.partial().exists(), "partial removed");
    let _ = std::fs::remove_dir_all(&root);
}

/// A relative scratch folder from the project file is not used.
#[test]
fn a_relative_scratch_folder_is_ignored() {
    let (mut s, item, root) = setup("autoproxy-scratch", 6);
    s.background.pause();
    pretend(&mut s, item, "HEVC", "YUV 4:2:2 10-bit", None);
    let mut p = (*s.project).clone();
    p.settings.scratch.captured = Some("../../somewhere".into());
    s.project = Arc::new(p);
    proxy_jobs::on_import(&mut s, &[item]);
    let st = status(&mut s);
    let path = st["items"][0]["path"].as_str().unwrap().to_string();
    assert!(Path::new(&path).starts_with(root.join("Proxies")), "{path}");
    proxy_jobs::cancel(&mut s, None);
    s.background.resume();
    let _ = std::fs::remove_dir_all(&root);
}

/// A step that panics (the worker catches it) ends as Failed, not Cancelled, and leaves no
/// partial file.
#[test]
fn a_panicking_step_ends_as_failed() {
    let (s, item, root) = setup("autoproxy-panic", 12);
    let out = root.join("Proxies").join(proxy_file_name("a", 9, "mov"));
    let mut job = ProxyJob::for_test(&s, item, &out, preset("prores_proxy_half").unwrap());
    assert_eq!(job.step(), Ok(crate::background::JobStep::More));
    job.panic_in_step = true;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.step()));
    assert!(r.is_err(), "it panicked");
    job.cancelled();
    assert!(job.outcome().starts_with("Failed"), "{}", job.outcome());
    assert!(!job.partial().exists());
    let _ = std::fs::remove_dir_all(&root);
}
