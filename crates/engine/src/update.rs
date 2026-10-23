//! Update check (fork UPD, FORK-PLAN §1a.77): at most once a day, in the background, ask GitHub's
//! releases API for the newest release of this build's channel and compare it with the running
//! build. Offline, rate-limited, malformed or hostile replies only set an error status (never a
//! crash, never a stall: the request runs on its own thread and the reply size is capped).
//!
//! The check sends one HTTPS request to `api.github.com` (GitHub sees the IP address); no usage
//! data, no identifiers. Builds without a [`Channel`] (upstream builds) never check.
//!
//! Commands: `app.update.check`, `app.update.status`, `app.update.setEnabled`, `app.update.install`,
//! `app.update.rollback` (the install itself: [`crate::update_install`]).

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad};
use crate::{EngineError, Result, Session};

/// Largest release list accepted (GitHub's 30 newest releases are ~100 KB).
pub const MAX_REPLY_BYTES: u64 = 2 << 20;
/// Most releases looked at in one reply.
const MAX_RELEASES: usize = 100;
/// Release notes longer than this are cut (characters).
const MAX_NOTES_CHARS: usize = 20_000;
/// Seconds between automatic checks.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Where a build looks for updates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channel {
    /// GitHub `owner/repo`.
    pub repo: &'static str,
    /// Only releases whose tag starts with this count (the repo holds other releases too).
    pub tag_prefix: &'static str,
}

impl Channel {
    pub fn releases_url(&self) -> String {
        format!("https://api.github.com/repos/{}/releases?per_page=30", self.repo)
    }

    /// Download links must point into this repo's releases.
    fn download_prefix(&self) -> String {
        format!("https://github.com/{}/releases/download/", self.repo)
    }
}

/// This build's channel: the fork's releases in fork builds, none upstream.
#[cfg(feature = "fork-update")] // FORK-ONLY (UPD, FORK-PLAN §1a.77): the fork's public releases
pub fn channel() -> Option<Channel> {
    Some(crate::fork_update::CHANNEL)
}

/// This build's channel: none (builds without an update channel never check).
#[cfg(not(feature = "fork-update"))]
pub fn channel() -> Option<Channel> {
    None
}

/// Settings ▸ General ▸ Updates (persisted as `update.*`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdatePrefs {
    /// Check for updates every day ("Don't check for updates" turns it off).
    pub check: bool,
    /// When the last check finished (seconds since 1970, UTC); 0 = never.
    pub last_check: u64,
    /// The first prompt (which says what the check sends) has been shown.
    pub explained: bool,
    /// The build an update replaced ("Roll back to …"); empty = none.
    pub previous: String,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self { check: true, last_check: 0, explained: false, previous: String::new() }
    }
}

/// A downloadable file of a release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// One release of the channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// Public tag (`fork-storage-1`).
    pub tag: String,
    pub name: String,
    pub notes: String,
    /// Seconds since 1970 (UTC).
    pub published: u64,
    /// The Linux build for this processor (`<tag>-linux-x86_64.tar.gz`), if the release has one.
    pub tarball: Option<Asset>,
    /// The AppImage for this processor (`<tag>-linux-x86_64.AppImage`, FORK-PLAN §1a.107), if any.
    pub appimage: Option<Asset>,
    /// The `SHA256SUMS` file, if the release has one.
    pub sums: Option<Asset>,
}

/// The processor name in release file names: `x86_64` or `aarch64` (FORK-PLAN §1a.107).
pub fn linux_arch() -> &'static str {
    std::env::consts::ARCH
}

/// The name of a release's Linux tarball for this processor (`revolution-0.1-linux-x86_64.tar.gz`;
/// fork rebrand, FORK-PLAN §1a.103: the tag already names the app).
pub fn tarball_name(tag: &str) -> String {
    format!("{tag}-linux-{}.tar.gz", linux_arch())
}

/// The name of a release's AppImage for this processor (`revolution-0.1.3-linux-x86_64.AppImage`).
pub fn appimage_name(tag: &str) -> String {
    format!("{tag}-linux-{}.AppImage", linux_arch())
}

/// Parses GitHub's release list, keeping the channel's published releases (drafts, pre-releases,
/// other prefixes, bad tags, bad dates and links outside the repo are left out), newest first.
pub fn parse_releases(body: &[u8], ch: &Channel) -> std::result::Result<Vec<Release>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("GitHub's reply isn't JSON: {e}"))?;
    let list = v.as_array().ok_or("GitHub's reply isn't a release list")?;
    let prefix = ch.download_prefix();
    let mut out: Vec<Release> = list
        .iter()
        .take(MAX_RELEASES)
        .filter_map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str);
            let tag = s("tag_name")?;
            if !tag.starts_with(ch.tag_prefix) || tag.len() == ch.tag_prefix.len() || !valid_tag(tag) {
                return None;
            }
            if r.get("draft").and_then(Value::as_bool).unwrap_or(true) || r.get("prerelease").and_then(Value::as_bool).unwrap_or(true) {
                return None;
            }
            let published = parse_time(s("published_at")?)?;
            let assets = r.get("assets").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
            let asset = |want: &str| {
                assets.iter().take(MAX_RELEASES).find_map(|a| {
                    let name = a.get("name").and_then(Value::as_str)?;
                    let url = a.get("browser_download_url").and_then(Value::as_str)?;
                    let size = a.get("size").and_then(Value::as_u64)?;
                    // exactly this release's file in this repo (no `..`, no encoded tricks)
                    (name == want && url == format!("{prefix}{tag}/{want}")).then(|| Asset { name: name.to_string(), url: url.to_string(), size })
                })
            };
            Some(Release {
                tag: tag.to_string(),
                name: s("name").unwrap_or(tag).chars().take(200).collect(),
                notes: s("body").unwrap_or("").chars().take(MAX_NOTES_CHARS).collect(),
                published,
                // releases published before the fork rebrand named it `filmcraft-<tag>-…`
                tarball: asset(&tarball_name(tag)).or_else(|| asset(&format!("filmcraft-{}", tarball_name(tag)))),
                appimage: asset(&appimage_name(tag)),
                sums: asset("SHA256SUMS"),
            })
        })
        .collect();
    out.sort_by(|a, b| b.published.cmp(&a.published).then_with(|| b.tag.cmp(&a.tag)));
    Ok(out)
}

/// Tags are short and plain (`fork-h264-3`): they become folder and file names.
pub fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.' || b == b'_')
        && !t.starts_with(['.', '-'])
        && !t.contains("..")
}

/// `YYYY-MM-DDTHH:MM:SSZ` (GitHub's format) → seconds since 1970.
pub fn parse_time(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    if b.len() != 20 || b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || b.get(10) != Some(&b'T') || b.get(19) != Some(&b'Z') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<u32>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // days from civil (Howard Hinnant's algorithm), y >= 1970 so everything stays positive
    let (y, mo, d) = (i64::from(y), i64::from(mo), i64::from(d));
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s);
    u64::try_from(secs).ok()
}

/// What the running build is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Current {
    /// Public tag (`fork-storage-1`), when known.
    pub tag: Option<String>,
    /// When the build was made (seconds since 1970), when known.
    pub built: Option<u64>,
}

impl Current {
    /// The running build: its tag from the build environment (`FILMCRAFT_RELEASE_TAG`) or from its
    /// folder (`<builds>/<tag>/filmcraft` or `filmcraft-<tag>/filmcraft`); its age from the
    /// executable's modification time.
    pub fn detect(ch: &Channel) -> Self {
        let exe = std::env::current_exe().ok();
        let from_env = option_env!("FILMCRAFT_RELEASE_TAG").map(str::to_string);
        let from_dir = exe.as_deref().and_then(|e| tag_from_dir(e, ch));
        let built = exe.as_deref().and_then(|e| std::fs::metadata(e).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
        Self { tag: from_env.filter(|t| valid_tag(t)).or(from_dir), built }
    }
}

/// `<builds>/storage-1/filmcraft` → `fork-storage-1`; `filmcraft-fork-yt-3/filmcraft` → `fork-yt-3`.
pub fn tag_from_dir(exe: &Path, ch: &Channel) -> Option<String> {
    let dir = exe.parent()?.file_name()?.to_str()?;
    let tag = match dir.strip_prefix("filmcraft-") {
        Some(t) => t.to_string(),
        None if dir.starts_with(ch.tag_prefix) => dir.to_string(),
        None => format!("{}{dir}", ch.tag_prefix),
    };
    (tag.starts_with(ch.tag_prefix) && tag.len() > ch.tag_prefix.len() && valid_tag(&tag)).then_some(tag)
}

/// Is there something newer than `cur` among `releases` (newest first)?
/// - the running tag is the newest release → no;
/// - the running tag is an older release → the newest;
/// - the running tag isn't released (a held build) → the newest only if it was published after
///   this build was made; without a build time, no (never offer an older build as an update).
pub fn newer<'a>(releases: &'a [Release], cur: &Current) -> Option<&'a Release> {
    newer_with(releases, cur, |_| true)
}

/// [`newer`] among the releases `usable` accepts (the ones carrying this install's file, LO.1):
/// a release without it (e.g. a tarball-only patch, or an AppImage missing) is never offered,
/// and the newest one that has it is offered instead.
pub fn newer_with<'a>(releases: &'a [Release], cur: &Current, usable: impl Fn(&Release) -> bool) -> Option<&'a Release> {
    let newest = releases.iter().find(|r| usable(r))?;
    if let Some(tag) = cur.tag.as_deref() {
        if newest.tag == tag {
            return None;
        }
        if let Some(mine) = releases.iter().find(|r| r.tag == tag) {
            return (newest.published > mine.published).then_some(newest);
        }
    }
    cur.built.filter(|&b| newest.published > b).map(|_| newest)
}

/// Is an automatic check due?
pub fn due(p: &UpdatePrefs, now: u64) -> bool {
    p.check && (now >= p.last_check.saturating_add(CHECK_INTERVAL_SECS) || p.last_check > now.saturating_add(CHECK_INTERVAL_SECS))
}

/// Fetches documents over HTTPS (a mock in tests).
pub trait Fetch: Send + Sync {
    /// GET `url`, at most `max_bytes` of body.
    fn get(&self, url: &str, max_bytes: u64) -> std::result::Result<Vec<u8>, String>;

    /// Streams `url` into the new file `dest` (at most `max_bytes`), calling `progress(done,
    /// total)` (false cancels). Returns the byte count; on failure `dest` is removed.
    fn download(&self, url: &str, dest: &Path, max_bytes: u64, progress: &mut dyn FnMut(u64, u64) -> bool) -> std::result::Result<u64, String> {
        let body = self.get(url, max_bytes)?;
        let n = body.len() as u64;
        let r = std::fs::OpenOptions::new().write(true).create_new(true).open(dest).and_then(|mut f| std::io::Write::write_all(&mut f, &body));
        if let Err(e) = r {
            let _ = std::fs::remove_file(dest);
            return Err(e.to_string());
        }
        if !progress(n, max_bytes) {
            let _ = std::fs::remove_file(dest);
            return Err("cancelled".into());
        }
        Ok(n)
    }
}

/// The real fetcher (pure-Rust TLS), or a stand-in that says this build can't go online.
pub struct Https;

impl Fetch for Https {
    #[cfg(feature = "speech-download")]
    fn get(&self, url: &str, max_bytes: u64) -> std::result::Result<Vec<u8>, String> {
        filmcraft_speech::models::https_get(url, "application/vnd.github+json", max_bytes).map_err(|e| e.to_string())
    }

    #[cfg(not(feature = "speech-download"))]
    fn get(&self, _url: &str, _max_bytes: u64) -> std::result::Result<Vec<u8>, String> {
        Err("this build can't go online".into())
    }

    #[cfg(feature = "speech-download")]
    fn download(&self, url: &str, dest: &Path, max_bytes: u64, progress: &mut dyn FnMut(u64, u64) -> bool) -> std::result::Result<u64, String> {
        filmcraft_speech::models::https_download_to(url, dest, max_bytes, &mut |done| progress(done, max_bytes)).map_err(|e| e.to_string())
    }
}

/// What the update check knows (`app.update.status`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `unsupported` (no channel) | `idle` | `checking` | `upToDate` | `available` | `error`.
    pub state: String,
    pub current: Current,
    /// The newer release, when `available`.
    pub latest: Option<Release>,
    /// The error, when `error` (offline, rate-limited, bad reply…) or a failed install.
    pub message: String,
    /// `none` | `installing` | `installed` | `failed` (one-click install).
    pub install: String,
    /// Bytes downloaded / to download while installing.
    pub done: u64,
    pub total: u64,
    /// The new build's program after `installed` (restart into it).
    pub installed: String,
    /// Why this build can't update itself (Flatpak, not installed by the build procedure, …);
    /// empty = it can.
    pub manual: String,
    /// When the last check finished (seconds since 1970).
    pub checked: u64,
}

type Outcome = std::result::Result<Vec<Release>, String>;

/// The update check's state in the session.
pub struct Updater {
    pub channel: Option<Channel>,
    pub fetch: Arc<dyn Fetch>,
    pub current: Current,
    pub status: Status,
    /// The finished (or running) background request.
    job: Option<Arc<Mutex<Option<Outcome>>>>,
    /// "Later": the prompt stays closed until the next check finds something.
    pub dismissed: bool,
    /// Where the builds live, or why this build can't update itself.
    pub layout: std::result::Result<crate::update_install::Layout, String>,
    install_job: Option<Arc<Mutex<InstallJob>>>,
}

#[derive(Default)]
struct InstallJob {
    done: u64,
    total: u64,
    result: Option<std::result::Result<std::path::PathBuf, String>>,
}

impl Default for Updater {
    fn default() -> Self {
        let channel = channel();
        let current = channel.as_ref().map(Current::detect).unwrap_or_default();
        let mut u = Self::new(channel, Arc::new(Https), current);
        u.set_layout(match std::env::current_exe() {
            Ok(exe) => crate::update_install::layout(&exe),
            Err(e) => Err(format!("can't find this build: {e}")),
        });
        u
    }
}

impl Updater {
    pub fn new(channel: Option<Channel>, fetch: Arc<dyn Fetch>, current: Current) -> Self {
        let state = if channel.is_some() { "idle" } else { "unsupported" };
        let status = Status { state: state.into(), current: current.clone(), install: "none".into(), ..Default::default() };
        let layout = Err("not looked at yet".to_string());
        let mut u = Self { channel, fetch, current, status, job: None, dismissed: false, layout, install_job: None };
        u.set_layout(Err("this build can't update itself".into()));
        u
    }

    /// Where the builds live (tests point it at a scratch folder).
    pub fn set_layout(&mut self, l: std::result::Result<crate::update_install::Layout, String>) {
        self.status.manual = l.as_ref().err().cloned().unwrap_or_default();
        self.layout = l;
    }

    /// Starts installing the offered release in the background.
    pub fn start_install(&mut self) -> std::result::Result<(), String> {
        let ch = self.channel.ok_or("this build doesn't check for updates")?;
        if self.install_job.is_some() {
            return Ok(());
        }
        if self.status.install == "installed" {
            return Err("the update is installed: restart Revolution to use it".into());
        }
        let rel = self.status.latest.clone().ok_or("there is no update to install: check first")?;
        let layout = self.layout.clone()?;
        let file = if layout.appimage.is_some() { rel.appimage.as_ref() } else { rel.tarball.as_ref() };
        let job = Arc::new(Mutex::new(InstallJob { total: file.map(|t| t.size).unwrap_or(0), ..Default::default() }));
        let (j2, fetch) = (job.clone(), self.fetch.clone());
        let run = move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::update_install::install(&*fetch, &rel, ch.tag_prefix, &layout, &mut |done, total| {
                    let mut j = j2.lock().unwrap_or_else(PoisonError::into_inner);
                    (j.done, j.total) = (done, total);
                    true
                })
            }))
            .unwrap_or_else(|_| Err("the update failed".into()));
            j2.lock().unwrap_or_else(PoisonError::into_inner).result = Some(r);
        };
        if cfg!(target_arch = "wasm32") || std::thread::Builder::new().name("filmcraft-update-install".into()).spawn(run).is_err() {
            return Err("can't start the update".into());
        }
        self.install_job = Some(job);
        self.status.install = "installing".into();
        self.status.message.clear();
        Ok(())
    }

    /// Collects install progress; returns the new build's folder once, when it is installed.
    pub fn poll_install(&mut self) -> Option<std::path::PathBuf> {
        let job = self.install_job.clone()?;
        let mut j = job.lock().unwrap_or_else(PoisonError::into_inner);
        (self.status.done, self.status.total) = (j.done, j.total);
        let r = j.result.take()?;
        drop(j);
        self.install_job = None;
        match r {
            Ok(dir) => {
                self.status.install = "installed".into();
                // an AppImage update returns the AppImage itself, a builds update its folder
                self.status.installed = if dir.is_file() {
                    dir.to_string_lossy().into_owned()
                } else {
                    crate::update_install::program_in(&dir).unwrap_or_else(|| dir.join(crate::update_install::PROGRAMS[0])).to_string_lossy().into_owned()
                };
                Some(dir)
            }
            Err(e) => {
                log::warn!("update install: {e}");
                self.status.install = "failed".into();
                self.status.message = e.chars().take(500).collect();
                None
            }
        }
    }

    /// Starts a background check unless one is running. False = no channel or no thread.
    pub fn start(&mut self) -> bool {
        let Some(ch) = self.channel else { return false };
        if self.job.is_some() {
            return true;
        }
        let slot: Arc<Mutex<Option<Outcome>>> = Arc::new(Mutex::new(None));
        let (s2, fetch) = (slot.clone(), self.fetch.clone());
        let run = move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let body = fetch.get(&ch.releases_url(), MAX_REPLY_BYTES)?;
                parse_releases(&body, &ch)
            }))
            .unwrap_or_else(|_| Err("the update check failed".into()));
            *s2.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        };
        if cfg!(target_arch = "wasm32") || std::thread::Builder::new().name("filmcraft-update-check".into()).spawn(run).is_err() {
            return false;
        }
        self.job = Some(slot);
        self.status.state = "checking".into();
        true
    }

    /// Collects a finished check (cheap; frontends call it every frame). True once, when a
    /// check finishes.
    pub fn poll(&mut self, now: u64) -> bool {
        let Some(job) = &self.job else { return false };
        let Some(r) = job.lock().unwrap_or_else(PoisonError::into_inner).take() else { return false };
        self.job = None;
        self.status.checked = now;
        match r {
            Ok(list) => {
                // Only a release carrying this install's file can be installed (LO.1); a build that
                // can't update itself is told about any newer release.
                let latest = match &self.layout {
                    Ok(l) if l.appimage.is_some() => newer_with(&list, &self.current, |r| r.appimage.is_some()),
                    Ok(_) => newer_with(&list, &self.current, |r| r.tarball.is_some()),
                    Err(_) => newer(&list, &self.current),
                }
                .cloned();
                if latest.as_ref().map(|l| &l.tag) != self.status.latest.as_ref().map(|l| &l.tag) {
                    self.dismissed = false;
                }
                self.status.state = if latest.is_some() { "available" } else { "upToDate" }.into();
                self.status.latest = latest;
                self.status.message.clear();
            }
            Err(e) => {
                log::info!("update check: {e}");
                self.status.state = "error".into();
                self.status.message = e.chars().take(500).collect();
            }
        }
        true
    }

    /// Should the frontend show the "Update available" prompt?
    pub fn prompt(&self) -> bool {
        self.status.state == "available" && !self.dismissed
    }
}

fn now_secs() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Session {
    /// The once-a-day automatic check (frontends call it at start-up and then now and then).
    pub fn auto_update_check(&mut self) -> bool {
        due(&self.prefs.update, now_secs()) && self.updater.start()
    }

    /// Collects a finished update check and records its time in the preferences.
    pub fn poll_update(&mut self) -> bool {
        let now = now_secs();
        if !self.updater.poll(now) {
            return false;
        }
        if self.updater.status.state != "error" {
            let mut next = self.prefs.clone();
            next.update.last_check = now;
            if let Err(e) = self.set_prefs(next) {
                log::warn!("saving the update check time: {e}");
            }
        }
        true
    }

    /// Collects install progress; remembers the replaced build for "Roll back".
    pub fn poll_update_install(&mut self) {
        if self.updater.poll_install().is_none() {
            return;
        }
        if let Ok(l) = &self.updater.layout {
            let mut next = self.prefs.clone();
            next.update.previous = match &l.appimage {
                Some(app) => crate::update_install::previous_appimage(app).to_string_lossy().into_owned(),
                None => l.current.to_string_lossy().into_owned(),
            };
            if let Err(e) = self.set_prefs(next) {
                log::warn!("saving the previous build: {e}");
            }
        }
    }

    fn update_status(&self) -> Value {
        let mut v = serde_json::to_value(&self.updater.status).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.insert("enabled".into(), json!(self.prefs.update.check));
            o.insert("explained".into(), json!(self.prefs.update.explained));
            o.insert("prompt".into(), json!(self.updater.prompt()));
            o.insert("sends".into(), json!(SENDS));
            o.insert("previous".into(), json!(self.prefs.update.previous));
        }
        v
    }
}

/// What the check sends, in the words the first prompt and Settings use.
pub const SENDS: &str =
    "One request to GitHub (api.github.com) asking for the newest release. GitHub sees your IP address; nothing else is sent: no usage data, no identifiers.";

fn check_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "app.update.check";
    if s.updater.channel.is_none() {
        return Err(bad(CMD, "this build doesn't check for updates"));
    }
    // a manual check ("Check now") runs even when automatic checks are off
    let force = p.get("force").and_then(Value::as_bool).unwrap_or(true);
    let started = if force { s.updater.start() } else { s.auto_update_check() };
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    if started && wait {
        // agents and tests: wait for the reply (bounded by the fetcher's own time limits + 5 s)
        let until = web_time::Instant::now() + std::time::Duration::from_secs(95);
        while !s.poll_update() && s.updater.status.state == "checking" && web_time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let mut v = s.update_status();
    if let Some(o) = v.as_object_mut() {
        o.insert("started".into(), json!(started));
    }
    Ok(v)
}

fn install_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "app.update.install";
    s.poll_update();
    s.updater.start_install().map_err(|e| bad(CMD, e))?;
    if p.get("wait").and_then(Value::as_bool).unwrap_or(false) {
        // agents and tests: wait for the install (a download can take a while: 30 min at most)
        let until = web_time::Instant::now() + std::time::Duration::from_secs(1800);
        while s.updater.status.install == "installing" && web_time::Instant::now() < until {
            s.poll_update_install();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    Ok(s.update_status())
}

fn rollback_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    const CMD: &str = "app.update.rollback";
    let prev = std::path::PathBuf::from(&s.prefs.update.previous);
    if s.prefs.update.previous.is_empty() {
        return Err(bad(CMD, "no earlier build to go back to"));
    }
    let (builds, appimage) = match &s.updater.layout {
        Ok(l) => (l.builds.clone(), l.appimage.clone()),
        Err(e) => return Err(bad(CMD, e.clone())),
    };
    if let Some(app) = appimage {
        // the AppImage the update replaced was kept as `<file>.previous`: move it back
        if prev != crate::update_install::previous_appimage(&app) || !prev.is_file() {
            return Err(bad(CMD, format!("the earlier AppImage isn't there any more ({})", prev.display())));
        }
        std::fs::rename(&prev, &app).map_err(|e| bad(CMD, format!("switching back: {e}")))?;
        let mut next = s.prefs.clone();
        next.update.previous.clear();
        if let Err(e) = s.set_prefs(next) {
            log::warn!("saving the update state: {e}");
        }
        let mut v = s.update_status();
        if let Some(o) = v.as_object_mut() {
            o.insert("rolledBack".into(), json!(app.to_string_lossy()));
        }
        return Ok(v);
    }
    let program = crate::update_install::program_in(&prev);
    let Some(program) = program.filter(|_| prev.parent() == Some(builds.as_path())) else {
        return Err(bad(CMD, format!("the earlier build isn't there any more ({})", prev.display())));
    };
    crate::update_install::switch_latest(&builds, &prev).map_err(|e| bad(CMD, format!("switching back: {e}")))?;
    let mut v = s.update_status();
    if let Some(o) = v.as_object_mut() {
        o.insert("rolledBack".into(), json!(program.to_string_lossy()));
    }
    Ok(v)
}

fn status_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    s.poll_update();
    s.poll_update_install();
    match p.get("dismiss").and_then(Value::as_str) {
        None => {}
        Some("later") => s.updater.dismissed = true,
        Some("explained") => {
            let mut next = s.prefs.clone();
            next.update.explained = true;
            s.set_prefs(next).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
        }
        Some(_) => return Err(bad("app.update.status", "`dismiss` is `later` or `explained`")),
    }
    Ok(s.update_status())
}

fn set_enabled_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "app.update.setEnabled";
    let on = p.get("enabled").and_then(Value::as_bool).ok_or_else(|| bad(CMD, "need `enabled` (true / false)"))?;
    let mut next = s.prefs.clone();
    next.update.check = on;
    next.update.explained = true;
    s.set_prefs(next).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
    if !on {
        s.updater.dismissed = true;
    }
    Ok(s.update_status())
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "app.update.check",
            label: "Check for Updates",
            menu: &[],
            shortcut: None,
            params: r#"{"force":bool?,"wait":bool?}"#,
            enabled: always,
            run: check_cmd,
            journal: false,
        },
        CommandSpec {
            id: "app.update.status",
            label: "Update Status",
            menu: &[],
            shortcut: None,
            params: r#"{"dismiss":str?}"#,
            enabled: always,
            run: status_cmd,
            journal: false,
        },
        CommandSpec {
            id: "app.update.install",
            label: "Install Update",
            menu: &[],
            shortcut: None,
            params: r#"{"wait":bool?}"#,
            enabled: always,
            run: install_cmd,
            journal: false,
        },
        CommandSpec {
            id: "app.update.rollback",
            label: "Roll Back Update",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: rollback_cmd,
            journal: false,
        },
        CommandSpec {
            id: "app.update.setEnabled",
            label: "Check for Updates Automatically",
            menu: &[],
            shortcut: None,
            params: r#"{"enabled":bool}"#,
            enabled: always,
            run: set_enabled_cmd,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
