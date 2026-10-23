use super::*;

const CH: Channel = Channel { repo: "dawgon2/filmcraft-cachyos", tag_prefix: "fork-" };

fn release(tag: &str, published: &str) -> Value {
    json!({
        "tag_name": tag,
        "name": format!("FilmCraft fork: {tag}"),
        "body": format!("notes for {tag}"),
        "draft": false,
        "prerelease": false,
        "published_at": published,
        "assets": [
            {"name": tarball_name(tag), "size": 54_175_216,
             "browser_download_url": format!("https://github.com/dawgon2/filmcraft-cachyos/releases/download/{tag}/{}", tarball_name(tag))},
            {"name": "SHA256SUMS", "size": 120,
             "browser_download_url": format!("https://github.com/dawgon2/filmcraft-cachyos/releases/download/{tag}/SHA256SUMS")}
        ]
    })
}

fn list() -> Vec<u8> {
    serde_json::to_vec(&json!([
        release("fork-yt-3", "2026-10-14T09:00:00Z"),
        release("explainer-v2", "2026-10-15T09:00:00Z"),
        release("fork-h264-3", "2026-10-15T20:00:00Z"),
        release("fork-comp-1", "2026-10-14T15:00:00Z"),
    ]))
    .unwrap()
}

fn cur(tag: Option<&str>, built: Option<&str>) -> Current {
    Current { tag: tag.map(str::to_string), built: built.and_then(parse_time) }
}

/// A mock release server: the reply it gives, or an error (offline / rate limited).
struct Mock(std::result::Result<Vec<u8>, String>);

impl Fetch for Mock {
    fn get(&self, url: &str, max_bytes: u64) -> std::result::Result<Vec<u8>, String> {
        assert_eq!(url, "https://api.github.com/repos/dawgon2/filmcraft-cachyos/releases?per_page=30");
        let body = self.0.clone()?;
        if body.len() as u64 > max_bytes {
            return Err(format!("the reply is larger than {max_bytes} bytes"));
        }
        Ok(body)
    }
}

struct Panics;

impl Fetch for Panics {
    fn get(&self, _: &str, _: u64) -> std::result::Result<Vec<u8>, String> {
        panic!("hostile fetcher")
    }
}

fn session(fetch: Arc<dyn Fetch>, current: Current) -> Session {
    let mut s = Session::new(Arc::new(crate::FsServices));
    s.updater = Updater::new(Some(CH), fetch, current);
    s
}

fn check(s: &mut Session) -> Value {
    s.execute("app.update.check", json!({"wait": true})).unwrap()
}

#[test]
fn times_parse_as_utc_seconds() {
    assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_time("2000-03-01T00:00:00Z"), Some(951_868_800));
    assert_eq!(parse_time("2024-02-29T12:00:00Z"), Some(1_709_208_000));
    assert_eq!(parse_time("2026-10-16T19:14:00Z"), Some(1_792_178_040));
    for bad in [
        "",
        "2026-13-01T00:00:00Z",
        "2026-10-16 19:14:00Z",
        "2026-10-16T19:14:00",
        "1969-12-31T23:59:59Z",
        "2026-10-16T24:00:00Z",
        "２026-10-16T19:14:0Z",
        "+026-10-16T19:14:00Z",
    ] {
        assert_eq!(parse_time(bad), None, "{bad}");
    }
}

#[test]
fn the_release_list_keeps_only_the_channels_published_releases_newest_first() {
    let mut v: Value = serde_json::from_slice(&list()).unwrap();
    let a = v.as_array_mut().unwrap();
    let mut draft = release("fork-draft-1", "2026-10-16T09:00:00Z");
    draft["draft"] = json!(true);
    let mut pre = release("fork-pre-1", "2026-10-16T09:00:00Z");
    pre["prerelease"] = json!(true);
    let mut elsewhere = release("fork-evil-1", "2026-10-16T09:00:00Z");
    elsewhere["assets"][0]["browser_download_url"] = json!("https://evil.example/filmcraft-fork-evil-1-linux-x86_64.tar.gz");
    let mut climb = release("fork-climb-1", "2026-10-16T08:00:00Z");
    climb["assets"][0]["browser_download_url"] = json!(
        "https://github.com/dawgon2/filmcraft-cachyos/releases/download/../../../evil/x/releases/download/fork-climb-1/filmcraft-fork-climb-1-linux-x86_64.tar.gz"
    );
    climb["assets"][1]["browser_download_url"] = json!("https://github.com/dawgon2/filmcraft-cachyos/releases/download/fork-other-1/SHA256SUMS");
    let mut long = release("fork-long-1", "2026-10-13T09:00:00Z");
    long["body"] = json!("x".repeat(100_000));
    a.extend([draft, pre, elsewhere, climb, long, release("fork-../../etc", "2026-10-16T09:00:00Z"), release("fork-", "2026-10-16T09:00:00Z")]);
    a.extend([release("fork-baddate-1", "yesterday"), json!({"tag_name": 5}), json!("junk"), json!(null)]);
    let r = parse_releases(&serde_json::to_vec(&v).unwrap(), &CH).unwrap();
    let tags: Vec<&str> = r.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["fork-evil-1", "fork-climb-1", "fork-h264-3", "fork-comp-1", "fork-yt-3", "fork-long-1"]);
    assert_eq!((r[1].tarball.as_ref(), r[1].sums.as_ref()), (None, None), "`..` links and another release's files are ignored");
    let evil = &r[0];
    assert_eq!(evil.tarball, None, "a download link outside the repo is ignored");
    assert!(evil.sums.is_some());
    assert_eq!(r[2].tarball.as_ref().unwrap().name, "fork-h264-3-linux-x86_64.tar.gz");
    assert_eq!(r[5].notes.chars().count(), MAX_NOTES_CHARS);
}

#[test]
fn malformed_replies_are_errors_not_crashes() {
    for body in [&b""[..], b"{", b"{\"message\":\"API rate limit exceeded\"}", b"null", &[0xff, 0xfe, 0x00]] {
        assert!(parse_releases(body, &CH).is_err(), "{body:?}");
    }
    // deeply nested JSON hits serde_json's recursion limit instead of the stack
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(parse_releases(deep.as_bytes(), &CH).is_err());
    assert_eq!(parse_releases(b"[]", &CH).unwrap(), vec![]);
}

/// LO.1: a release without this install's file (a tarball-only patch, a missing AppImage) is skipped
/// and the newest release that has it is offered.
#[test]
fn newer_skips_releases_without_this_installs_file() {
    let mut r = parse_releases(&list(), &CH).unwrap();
    assert_eq!(r[0].tag, "fork-h264-3");
    assert_eq!(r[1].tag, "fork-comp-1");
    r[0].tarball = None;
    let has_tarball = |x: &Release| x.tarball.is_some();
    assert_eq!(newer_with(&r, &cur(Some("fork-yt-3"), None), has_tarball).map(|r| r.tag.as_str()), Some("fork-comp-1"));
    assert_eq!(newer_with(&r, &cur(Some("fork-comp-1"), None), has_tarball), None);
    // the running build is newer than every usable release → nothing
    assert_eq!(newer_with(&r, &cur(Some("fork-h264-3"), None), has_tarball), None);
    // a held build made between them → nothing older offered
    assert_eq!(newer_with(&r, &cur(Some("fork-ai-3"), Some("2026-10-15T16:00:00Z")), has_tarball), None);
    // nothing usable → nothing; without a filter the newest is still offered
    assert_eq!(newer_with(&r, &cur(Some("fork-yt-3"), None), |_| false), None);
    assert_eq!(newer(&r, &cur(Some("fork-yt-3"), None)).map(|r| r.tag.as_str()), Some("fork-h264-3"));
}

#[test]
fn newer_compares_with_the_running_build() {
    let r = parse_releases(&list(), &CH).unwrap();
    // same: the newest release
    assert_eq!(newer(&r, &cur(Some("fork-h264-3"), None)), None);
    // older release → the newest
    assert_eq!(newer(&r, &cur(Some("fork-yt-3"), None)).map(|r| r.tag.as_str()), Some("fork-h264-3"));
    // a held build made after the newest release (storage-1) → nothing
    assert_eq!(newer(&r, &cur(Some("fork-storage-1"), Some("2026-10-16T23:00:00Z"))), None);
    // a held build made before it → the newest
    assert_eq!(newer(&r, &cur(Some("fork-ai-3"), Some("2026-10-15T10:00:00Z"))).map(|r| r.tag.as_str()), Some("fork-h264-3"));
    // unknown tag and age → never offer anything
    assert_eq!(newer(&r, &cur(None, None)), None);
    assert_eq!(newer(&[], &cur(Some("fork-yt-3"), None)), None);
}

#[test]
fn the_tag_comes_from_the_build_folder() {
    let t = |p: &str| tag_from_dir(Path::new(p), &CH);
    assert_eq!(t("/home/u/Apps/filmcraft-builds/storage-1/filmcraft").as_deref(), Some("fork-storage-1"));
    assert_eq!(t("/opt/filmcraft-fork-yt-3/filmcraft").as_deref(), Some("fork-yt-3"));
    assert_eq!(t("/opt/fork-ai-5/filmcraft").as_deref(), Some("fork-ai-5"));
    assert_eq!(t("/home/u/Projects/filmcraft-main/target/release/filmcraft").as_deref(), Some("fork-release"));
    assert_eq!(t("/opt/Odd Name/filmcraft"), None);
    assert_eq!(t("filmcraft"), None);
}

#[test]
fn checks_are_due_once_a_day_and_only_when_on() {
    let p = UpdatePrefs::default();
    assert!(p.check && due(&p, 1_000_000));
    let p = UpdatePrefs { last_check: 1_000_000, ..Default::default() };
    assert!(!due(&p, 1_000_000 + CHECK_INTERVAL_SECS - 1));
    assert!(due(&p, 1_000_000 + CHECK_INTERVAL_SECS));
    // the clock went back by more than a day: check again rather than never
    assert!(due(&p, 1_000_000 - CHECK_INTERVAL_SECS - 1));
    assert!(!due(&UpdatePrefs { check: false, ..Default::default() }, 1_000_000));
    assert!(!due(&UpdatePrefs { last_check: u64::MAX, ..Default::default() }, u64::MAX - 1));
}

/// UPD accept (mock release server): new → available with notes; the check time is recorded.
#[test]
fn a_newer_release_is_offered_with_its_notes() {
    let mut s = session(Arc::new(Mock(Ok(list()))), cur(Some("fork-yt-3"), None));
    let v = check(&mut s);
    assert_eq!(v["state"], "available", "{v}");
    assert_eq!(v["latest"]["tag"], "fork-h264-3");
    assert_eq!(v["latest"]["notes"], "notes for fork-h264-3");
    assert_eq!(v["prompt"], true);
    assert_eq!(v["sends"], SENDS);
    assert!(s.prefs.update.last_check > 0);
    // Later: the prompt closes, the status stays
    let v = s.execute("app.update.status", json!({"dismiss": "later"})).unwrap();
    assert_eq!((v["state"].as_str(), v["prompt"].as_bool()), (Some("available"), Some(false)));
    // the next check finding the same release keeps it closed
    let v = check(&mut s);
    assert_eq!(v["prompt"], false);
}

/// UPD accept (mock release server): same / older → up to date, no prompt.
#[test]
fn the_same_or_an_older_release_is_up_to_date() {
    for tag in ["fork-h264-3", "fork-storage-1"] {
        let mut s = session(Arc::new(Mock(Ok(list()))), cur(Some(tag), Some("2026-10-16T23:00:00Z")));
        let v = check(&mut s);
        assert_eq!((v["state"].as_str(), v["prompt"].as_bool()), (Some("upToDate"), Some(false)), "{tag}: {v}");
    }
}

/// UPD accept (mock release server): offline / rate limited / malformed / oversized / a panicking
/// fetcher → an error status, no crash, no prompt, and the check time isn't recorded (it retries).
#[test]
fn failed_checks_are_quiet_errors() {
    let huge = vec![b' '; (MAX_REPLY_BYTES + 1) as usize];
    let fetchers: Vec<Arc<dyn Fetch>> = vec![
        Arc::new(Mock(Err("api.github.com: connection refused".into()))),
        Arc::new(Mock(Ok(br#"{"message":"API rate limit exceeded for 1.2.3.4."}"#.to_vec()))),
        Arc::new(Mock(Ok(b"<html>".to_vec()))),
        Arc::new(Mock(Ok(huge))),
        Arc::new(Panics),
    ];
    for f in fetchers {
        let mut s = session(f, cur(Some("fork-yt-3"), None));
        let v = check(&mut s);
        assert_eq!(v["state"], "error", "{v}");
        assert!(!v["message"].as_str().unwrap().is_empty());
        assert_eq!(v["prompt"], false);
        assert_eq!(s.prefs.update.last_check, 0);
    }
}

#[test]
fn turning_checks_off_stops_automatic_checks_but_not_check_now() {
    let mut s = session(Arc::new(Mock(Ok(list()))), cur(Some("fork-yt-3"), None));
    let v = s.execute("app.update.setEnabled", json!({"enabled": false})).unwrap();
    assert_eq!((v["enabled"].as_bool(), v["explained"].as_bool()), (Some(false), Some(true)));
    assert!(!s.auto_update_check());
    let v = s.execute("app.update.check", json!({"force": false, "wait": true})).unwrap();
    assert_eq!((v["started"].as_bool(), v["state"].as_str()), (Some(false), Some("idle")));
    // Check now
    assert_eq!(check(&mut s)["state"], "available");
    assert!(s.execute("app.update.setEnabled", json!({})).is_err());
    assert!(s.execute("app.update.status", json!({"dismiss": "forever"})).is_err());
    let v = s.execute("app.update.status", json!({"dismiss": "explained"})).unwrap();
    assert_eq!(v["explained"], true);
}

#[test]
fn builds_without_a_channel_never_check() {
    let mut s = Session::new(Arc::new(crate::FsServices));
    s.updater = Updater::new(None, Arc::new(Panics), Current::default());
    assert!(s.execute("app.update.check", json!({})).is_err());
    assert!(!s.auto_update_check());
    assert_eq!(s.execute("app.update.status", json!({})).unwrap()["state"], "unsupported");
}

#[test]
fn update_prefs_round_trip_and_default_on() {
    let p: crate::autosave::Preferences = serde_json::from_value(json!({})).unwrap();
    assert!(p.update.check && !p.update.explained);
    let mut s = Session::new(Arc::new(crate::FsServices));
    s.execute("prefs.set", json!({"key": "update.check", "value": false})).unwrap();
    assert!(!s.prefs.update.check);
}

/// A recorded reply of the real API (2026-10-16, trimmed to the fields read): the fork's 22
/// releases incl. explainer models; the newest `fork-` one is h264-3; none had SHA256SUMS yet.
#[test]
fn the_recorded_github_reply_parses() {
    let r = parse_releases(include_bytes!("update_tests_github.json"), &CH).unwrap();
    assert!(r.iter().all(|r| r.tag.starts_with("fork-")));
    assert_eq!(r[0].tag, "fork-h264-3");
    assert_eq!(r[0].tarball.as_ref().map(|t| t.size), Some(54_175_216));
    assert!(r.iter().all(|r| r.sums.is_none()));
    // the running held build storage-1 (made after h264-3) is up to date; yt-3 is offered h264-3
    assert_eq!(newer(&r, &cur(Some("fork-storage-1"), Some("2026-10-16T23:00:00Z"))), None);
    assert_eq!(newer(&r, &cur(Some("fork-yt-3"), None)).map(|r| r.tag.as_str()), Some("fork-h264-3"));
}
