//! **Project Manager** home screen (fork RESOLVE-UI-PLAN RS2, modelled on DaVinci Resolve's
//! Project Manager; generic, so it can go upstream).
//!
//! Resolve keeps its projects in a database; FilmCraft has `.fcproj` files, so the screen shows a
//! **projects folder** (Settings ▸ General ▸ Projects folder, default `~/Videos/FilmCraft
//! Projects`, created on first use; its sub-folders are the screen's folders) plus the recent
//! projects that live elsewhere ("Other locations").
//!
//! - **Summaries** (name, dates, resolution, frame rate, sequence count) are read from a project
//!   file without opening it, and cached in the data directory (`Project Cache/<hash>.json`,
//!   keyed by a hash of the canonical path and checked against the file's size and modification
//!   time: a stale or missing entry is recomputed from the file). Every save writes a fresh entry.
//! - **Posters**: the frontend renders the frame under the playhead after a save
//!   ([`Session::poster_requests`]) and stores it with [`store_poster`] (≤ 480 px PNG, next to the
//!   summary, with the page the project was saved on).
//! - **Hostile input**: project files and anything else in the folder are untrusted. Files are
//!   read up to [`MAX_PROJECT_BYTES`], a file that can't be read becomes an entry with `error`
//!   (the screen's "can't read" tile), never a failed listing. Names given to rename / new
//!   folder / new project are single path components ([`check_name`]); folders are relative to
//!   the projects folder and can't climb out of it.
//! - **Trash** moves to the operating system's trash (freedesktop.org trash on Linux / BSD,
//!   `~/.Trash` on macOS); nothing is ever deleted permanently.
//!
//! Commands: `projects.show`, `projects.list`, `projects.open`, `projects.new`, `projects.rename`,
//! `projects.duplicate`, `projects.newFolder`, `projects.reveal`, `projects.removeFromList`,
//! `projects.locate`, `projects.trash`, `projects.export`, `projects.import`.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p, u64_p};
use crate::{EngineError, Result, Session};

/// Largest project file read for a summary (and imported).
pub const MAX_PROJECT_BYTES: u64 = 64 << 20;
/// Largest poster file read back.
pub const MAX_POSTER_BYTES: u64 = 8 << 20;
/// Widest poster written.
pub const POSTER_WIDTH: u32 = 480;
/// Project file extension.
pub const EXT: &str = "fcproj";
/// Folder of the summary / poster cache in the data directory.
pub const CACHE_DIR: &str = "Project Cache";
/// Entries listed per folder at most (a folder with more shows the first ones by name).
pub const MAX_ENTRIES: usize = 5000;
/// Longest name (bytes) of a project or folder.
pub const MAX_NAME: usize = 200;
/// Folders FilmCraft keeps next to projects; not shown as folders of the screen.
pub const SUPPORT_DIRS: &[&str] = &[filmcraft_format::autosave::AUTO_SAVE_DIR, "FilmCraft Previews", "FilmCraft Stills", "Proxies"];
/// Poster requests kept for the frontend at most.
const MAX_POSTER_REQUESTS: usize = 16;

/// Settings ▸ General ▸ At startup show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StartupShow {
    ProjectManager,
    LastProject,
    NewEmpty,
}

impl StartupShow {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "projectManager" => Some(Self::ProjectManager),
            "lastProject" => Some(Self::LastProject),
            "newEmpty" => Some(Self::NewEmpty),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectManager => "projectManager",
            Self::LastProject => "lastProject",
            Self::NewEmpty => "newEmpty",
        }
    }
}

/// What the desktop app does at launch when no project was named on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupAction {
    /// The Project Manager home screen (with an empty project behind it).
    ProjectManager,
    /// Open the most recent project that still exists (the demo project when there is none).
    LastProject,
    /// An empty project.
    NewEmpty,
    /// The demo project (upstream's "Show Home").
    Demo,
}

/// The launch behaviour: `startup_flag` = `--demo` / `--empty` was given (they win), otherwise
/// Settings ▸ General ▸ At startup show; its "workflow default" is the Project Manager in the
/// Resolve workflow and Premiere's own At Startup choice in the Premiere workflow.
pub fn startup_action(prefs: &crate::autosave::Preferences, startup_flag: bool, demo_flag: bool) -> StartupAction {
    if startup_flag {
        return if demo_flag { StartupAction::Demo } else { StartupAction::NewEmpty };
    }
    match prefs.general.startup_show {
        Some(StartupShow::ProjectManager) => StartupAction::ProjectManager,
        Some(StartupShow::LastProject) => StartupAction::LastProject,
        Some(StartupShow::NewEmpty) => StartupAction::NewEmpty,
        None if prefs.workflow == crate::settings::Workflow::Resolve => StartupAction::ProjectManager,
        None => match prefs.general.at_startup.as_str() {
            "openMostRecent" => StartupAction::LastProject,
            "emptyProject" => StartupAction::NewEmpty,
            _ => StartupAction::Demo,
        },
    }
}

/// Serde for `general.startupShow`: `None` (the workflow default) is written as
/// `"workflowDefault"`; anything unknown reads as `None`, so a bad value never fails the whole
/// preferences file.
pub(crate) mod startup_show_serde {
    use super::StartupShow;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<StartupShow>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v.map_or("workflowDefault", StartupShow::as_str))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<StartupShow>, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(v.as_str().and_then(StartupShow::parse))
    }
}

/// Serde for `general.projectsFolder`: `None` (the default folder) is written as `""`.
pub(crate) mod folder_serde {
    use std::path::PathBuf;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from))
    }
}

// ------------------------------------------------------------------ summaries

/// What the home screen shows about one project, read without opening it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectSummary {
    /// The file name without `.fcproj` (FilmCraft names a project after its file).
    pub name: String,
    pub path: String,
    /// Seconds since 1970 (None when the file system doesn't say).
    pub modified: Option<u64>,
    pub created: Option<u64>,
    /// The active (else the first) sequence's frame size and rate; 0 without a sequence.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub sequence_count: usize,
    /// The page (Resolve workflow) the project was last saved on, when known.
    pub page: Option<String>,
    /// File size in bytes.
    pub bytes: u64,
    /// Why the file can't be read (the "can't read" tile); None when it was read.
    pub error: Option<String>,
    /// The file doesn't exist (any more): a recent project that moved or was deleted.
    pub missing: bool,
    /// The poster picture (PNG) in the cache, when there is one.
    pub poster: Option<String>,
}

/// A sub-folder of the shown folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FolderEntry {
    pub name: String,
    pub path: String,
    /// The folder relative to the projects folder (`a/b`), for `folder` params.
    pub folder: String,
    pub modified: Option<u64>,
    pub created: Option<u64>,
    /// Projects directly inside.
    pub projects: usize,
}

/// `projects.list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Listing {
    /// The projects folder.
    pub root: String,
    /// The shown folder relative to `root` (`""` = the projects folder itself).
    pub folder: String,
    pub path: String,
    /// The folder's names from the projects folder down (the breadcrumb after "Projects").
    pub breadcrumb: Vec<String>,
    /// Whether the folder exists (the projects folder is created on first use).
    pub exists: bool,
    pub folders: Vec<FolderEntry>,
    pub projects: Vec<ProjectSummary>,
    /// Recent projects outside the projects folder (missing ones flagged), shown on the top level.
    pub other: Vec<ProjectSummary>,
    /// Why the Projects folder preference isn't used (the default folder is shown instead).
    pub warning: Option<String>,
}

/// Sort order of `projects.list`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    #[default]
    Name,
    Modified,
    Created,
}

impl SortKey {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "name" => Some(Self::Name),
            "modified" => Some(Self::Modified),
            "created" => Some(Self::Created),
            _ => None,
        }
    }
}

fn unix(t: std::io::Result<SystemTime>) -> Option<u64> {
    t.ok()?.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn stem_of(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Whether `path` names a project file (by its extension, any case).
pub fn is_project_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXT))
}

fn finite(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 { x } else { 0.0 }
}

/// Sequence facts of a loaded project: (width, height, fps, sequence count).
fn sequence_facts(p: &filmcraft_project::Project, active: Option<filmcraft_project::ItemId>) -> (u32, u32, f64, usize) {
    let count = p.sequences().count();
    let seq = active.and_then(|a| p.sequence(a)).or_else(|| p.sequences().next().and_then(|i| p.sequence(i.id)));
    match seq {
        Some(q) => (q.settings.width, q.settings.height, finite(q.settings.frame_rate.as_f64()), count),
        None => (0, 0, 0.0, count),
    }
}

/// Read a project file's summary (capped; never fails: problems become `error` / `missing`).
pub fn read_summary(path: &Path) -> ProjectSummary {
    let mut s = ProjectSummary { name: stem_of(path), path: path.to_string_lossy().into_owned(), ..Default::default() };
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            s.missing = true;
            s.error = Some("the project file is missing".into());
            return s;
        }
        Err(e) => {
            s.error = Some(e.to_string());
            return s;
        }
    };
    s.modified = unix(meta.modified());
    s.created = unix(meta.created());
    s.bytes = meta.len();
    if !meta.is_file() {
        s.error = Some("not a project file".into());
        return s;
    }
    if meta.len() > MAX_PROJECT_BYTES {
        s.error = Some(format!("the file is larger than {} MiB", MAX_PROJECT_BYTES >> 20));
        return s;
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(MAX_PROJECT_BYTES + 1).read_to_end(&mut bytes));
    if let Err(e) = read {
        s.error = Some(e.to_string());
        return s;
    }
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        s.error = Some(format!("the file is larger than {} MiB", MAX_PROJECT_BYTES >> 20));
        return s;
    }
    match decode_guarded(&bytes) {
        Ok(l) => {
            let active = l.view.as_ref().and_then(|v| v.active_sequence);
            (s.width, s.height, s.fps, s.sequence_count) = sequence_facts(&l.project, active);
        }
        Err(e) => s.error = Some(e),
    }
    s
}

/// Decode untrusted project bytes; a decoder bug on a hostile file is an error, not a crash
/// (last-resort guard, shared by summaries and import).
fn decode_guarded(bytes: &[u8]) -> std::result::Result<filmcraft_format::Loaded, String> {
    match std::panic::catch_unwind(|| filmcraft_format::decode(bytes)) {
        Ok(Ok(l)) => Ok(l),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the project file could not be read".into()),
    }
}

// ------------------------------------------------------------------ cache

/// FNV-1a (stable across builds, unlike `DefaultHasher`).
fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The canonical form of a path (the path itself when it can't be resolved, e.g. it's gone).
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Where the directory entry `path` itself is: its folder resolved, the last part kept as is (a
/// link stays the link, not its target).
fn entry_path(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => {
            let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
            canonical(dir).join(name)
        }
        _ => canonical(path),
    }
}

/// Whether anything (a file, a folder, even a dangling link) is at `path`.
fn exists_entry(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The cache file stem of a project path.
pub fn cache_key(path: &Path) -> String {
    format!("{:016x}", fnv(canonical(path).as_os_str().as_encoded_bytes()))
}

/// The summary / poster cache folder of a session (None without a data directory: no cache).
pub fn cache_dir(s: &Session) -> Option<PathBuf> {
    let data = s.persistence.as_ref().map(|p| p.data_dir.clone()).or_else(|| s.prefs_path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)))?;
    Some(data.join(CACHE_DIR))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct CacheEntry {
    path: String,
    /// The file's modification time (ns since 1970) and size when the summary was taken.
    mtime_ns: u128,
    size: u64,
    summary: ProjectSummary,
    page: Option<String>,
}

fn mtime_ns(meta: &std::fs::Metadata) -> u128 {
    meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > cap {
        return None;
    }
    let mut b = Vec::new();
    std::fs::File::open(path).ok()?.take(cap + 1).read_to_end(&mut b).ok()?;
    (b.len() as u64 <= cap).then_some(b)
}

fn read_entry(cache: &Path, key: &str) -> Option<CacheEntry> {
    let b = read_capped(&cache.join(format!("{key}.json")), 1 << 20)?;
    serde_json::from_slice(&b).ok()
}

fn write_entry(cache: &Path, key: &str, e: &CacheEntry) -> std::io::Result<()> {
    std::fs::create_dir_all(cache)?;
    let b = serde_json::to_vec(e).map_err(std::io::Error::other)?;
    filmcraft_format::atomic_write(&cache.join(format!("{key}.json")), &b)
}

/// The summary of `path`, from the cache when its entry matches the file (same canonical path,
/// size and modification time), otherwise read from the file and cached.
pub fn summary(cache: Option<&Path>, path: &Path) -> ProjectSummary {
    let meta = std::fs::metadata(path).ok();
    let key = cache_key(path);
    let canon = canonical(path).to_string_lossy().into_owned();
    let entry = cache.and_then(|c| read_entry(c, &key));
    let mut s = match (&meta, &entry) {
        (Some(m), Some(e)) if e.path == canon && e.size == m.len() && e.mtime_ns == mtime_ns(m) && m.is_file() => {
            let mut s = e.summary.clone();
            // the cached path is canonical; show the path as listed
            s.path = path.to_string_lossy().into_owned();
            s.name = stem_of(path);
            s
        }
        _ => {
            let s = read_summary(path);
            if let (Some(c), Some(m)) = (cache, &meta)
                && !s.missing
            {
                let e = CacheEntry { path: canon, mtime_ns: mtime_ns(m), size: m.len(), summary: s.clone(), page: entry.as_ref().and_then(|e| e.page.clone()) };
                let _ = write_entry(c, &key, &e);
            }
            s
        }
    };
    s.page = entry.as_ref().and_then(|e| e.page.clone());
    s.poster = cache.map(|c| c.join(format!("{key}.png"))).filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned());
    s
}

/// After a save of the session's project to `path`: cache its summary (from memory, no re-read)
/// and ask the frontend for a poster. Best effort.
pub fn note_saved(s: &mut Session, path: &str) {
    let p = Path::new(path);
    if let Some(cache) = cache_dir(s)
        && let Ok(meta) = std::fs::metadata(p)
    {
        let key = cache_key(p);
        let page = read_entry(&cache, &key).and_then(|e| e.page);
        let (width, height, fps, sequence_count) = sequence_facts(&s.project, s.state.active_sequence);
        let summary = ProjectSummary {
            name: stem_of(p),
            path: path.to_string(),
            modified: unix(meta.modified()),
            created: unix(meta.created()),
            width,
            height,
            fps,
            sequence_count,
            bytes: meta.len(),
            ..Default::default()
        };
        let e = CacheEntry { path: canonical(p).to_string_lossy().into_owned(), mtime_ns: mtime_ns(&meta), size: meta.len(), summary, page };
        if let Err(e) = write_entry(&cache, &key, &e) {
            log::debug!("project summary cache: {e}");
        }
    }
    s.poster_requests.retain(|x| x != path);
    s.poster_requests.push(path.to_string());
    if s.poster_requests.len() > MAX_POSTER_REQUESTS {
        s.poster_requests.remove(0);
    }
}

/// Scale RGBA8 pixels down to at most `max_w` wide (box filter), keeping the aspect ratio.
pub fn downscale(w: u32, h: u32, rgba: &[u8], max_w: u32) -> Option<(u32, u32, Vec<u8>)> {
    let (wu, hu) = (w as usize, h as usize);
    if w == 0 || h == 0 || rgba.len() != wu.checked_mul(hu)?.checked_mul(4)? {
        return None;
    }
    if w <= max_w {
        return Some((w, h, rgba.to_vec()));
    }
    let nw = max_w.max(1);
    let nh = ((u64::from(h) * u64::from(nw)) / u64::from(w)).max(1) as u32;
    let (nwu, nhu) = (nw as usize, nh as usize);
    let mut out = vec![0u8; nwu * nhu * 4];
    for y in 0..nhu {
        let (y0, y1) = (y * hu / nhu, ((y + 1) * hu / nhu).max(y * hu / nhu + 1).min(hu));
        for x in 0..nwu {
            let (x0, x1) = (x * wu / nwu, ((x + 1) * wu / nwu).max(x * wu / nwu + 1).min(wu));
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = (yy * wu + xx) * 4;
                    if let Some(px) = rgba.get(i..i + 4) {
                        acc.iter_mut().zip(px).for_each(|(a, v)| *a += u64::from(*v));
                        n += 1;
                    }
                }
            }
            let o = (y * nwu + x) * 4;
            if let Some(dst) = out.get_mut(o..o + 4) {
                dst.iter_mut().zip(acc).for_each(|(d, a)| *d = (a / n.max(1)) as u8);
            }
        }
    }
    Some((nw, nh, out))
}

/// Store the poster of the project saved at `project` (RGBA8 `w`×`h`, scaled to ≤ 480 px) and
/// the page it was saved on.
pub fn store_poster(cache: &Path, project: &Path, w: u32, h: u32, rgba: &[u8], page: Option<&str>) -> std::io::Result<()> {
    let (pw, ph, px) = downscale(w, h, rgba, POSTER_WIDTH).ok_or_else(|| std::io::Error::other("the poster picture is malformed"))?;
    let img = image::RgbaImage::from_raw(pw, ph, px).ok_or_else(|| std::io::Error::other("the poster picture is malformed"))?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(cache)?;
    let key = cache_key(project);
    filmcraft_format::atomic_write(&cache.join(format!("{key}.png")), &png)?;
    if let Some(page) = page {
        let mut e = read_entry(cache, &key).unwrap_or_default();
        e.page = Some(page.to_string());
        write_entry(cache, &key, &e)?;
    }
    Ok(())
}

/// Make the poster of a project that has none (fork, owner 2026-10-20, FORK-PLAN §1a.108: every
/// project in the Project Manager shows a picture of its timeline): open it in a session of its
/// own (nothing is saved, no recent list or auto-save touched), render its active timeline at a
/// frame that shows something and store it with [`store_poster`]. The frame is the saved
/// playhead's, else one second into the first video clip, else the middle of the timeline; a
/// black frame moves on to the next candidate. Slow (it decodes media): run it off the UI thread.
pub fn make_poster(services: std::sync::Arc<dyn crate::Services>, cache: &Path, project: &Path) -> std::result::Result<(), String> {
    let mut s = Session::new(services);
    s.execute("file.open", json!({"path": project.to_string_lossy()})).map_err(|e| e.to_string())?;
    let seq_id = s.state.active_sequence.ok_or("the project has no timeline")?;
    let seq = s.project.sequence(seq_id).ok_or("the project has no timeline")?;
    let width = seq.settings.width.max(1);
    let duration = seq.duration();
    if duration <= filmcraft_time::Tick::ZERO {
        return Err("the timeline is empty".into());
    }
    let second = filmcraft_time::Tick(filmcraft_time::TICKS_PER_SECOND);
    let first_clip = seq
        .video_tracks
        .iter()
        .flat_map(|t| t.items.iter())
        .filter(|i| i.enabled && i.duration > filmcraft_time::Tick::ZERO)
        .min_by_key(|i| i.start)
        .map(|i| filmcraft_time::Tick(i.start.0.saturating_add(second.0.min(i.duration.0 / 2))));
    let middle = filmcraft_time::Tick(duration.0 / 2);
    let scale = (POSTER_WIDTH.min(width) as f32 / width as f32).clamp(0.01, 1.0);
    let mut fallback = None;
    for t in [Some(s.playhead()), first_clip, Some(middle)].into_iter().flatten() {
        let Some(img) = s.render_program_at(scale, t) else { continue };
        let px = img.over_black_rgba8();
        let (w, h) = (u32::try_from(img.w).map_err(|e| e.to_string())?, u32::try_from(img.h).map_err(|e| e.to_string())?);
        // a frame with nothing on it (black, a gap) is a poor poster: try the next one
        let lit = px.as_chunks::<4>().0.iter().filter(|p| p.iter().take(3).any(|c| *c > 16)).count();
        if lit * 50 > px.len() / 4 {
            return store_poster(cache, project, w, h, &px, None).map_err(|e| e.to_string());
        }
        fallback.get_or_insert((w, h, px));
    }
    let (w, h, px) = fallback.ok_or("no frame of the timeline could be rendered")?;
    store_poster(cache, project, w, h, &px, None).map_err(|e| e.to_string())
}

/// A poster PNG decoded to RGBA8 (capped size and dimensions; None when it can't be read).
pub fn load_poster(png_path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let bytes = read_capped(png_path, MAX_POSTER_BYTES)?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 << 20);
    reader.limits(limits);
    let img = reader.decode().ok()?.to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

// ------------------------------------------------------------------ folders

/// The default projects folder: `~/Videos/FilmCraft Projects`.
pub fn default_folder(home: Option<&str>) -> Option<PathBuf> {
    home.filter(|h| !h.is_empty()).map(|h| Path::new(h).join("Videos").join("FilmCraft Projects"))
}

/// Why `path` can't be the projects folder (None: it can): it must be absolute and narrower
/// than the home folder, since the Project Manager renames and trashes folders inside it.
pub fn unsafe_root(path: &Path, homes: &[PathBuf]) -> Option<String> {
    if !path.is_absolute() {
        return Some(format!("{} is not an absolute path", path.display()));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Some(format!("{} contains `..`", path.display()));
    }
    let c = canonical(path);
    if path.parent().is_none() || c.parent().is_none() {
        return Some(format!("{} is the top of the file system", path.display()));
    }
    for h in homes.iter().filter(|h| !h.as_os_str().is_empty()) {
        if h.starts_with(path) || canonical(h).starts_with(&c) {
            return Some(format!("{} is your home folder or holds it", path.display()));
        }
    }
    None
}

fn homes(s: &Session) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = s.services.home_dir().into_iter().map(PathBuf::from).collect();
    if let Some(h) = crate::media_browser::std_home_dir().map(PathBuf::from)
        && !v.contains(&h)
    {
        v.push(h);
    }
    v
}

/// Why Settings ▸ General ▸ Projects folder is not used (None when it's unset or usable).
pub fn root_warning(s: &Session) -> Option<String> {
    let p = s.prefs.general.projects_folder.as_ref()?;
    unsafe_root(p, &homes(s)).map(|why| format!("Projects folder not used: {why}; showing the default folder instead"))
}

/// The projects folder (Settings ▸ General ▸ Projects folder when it is safe, else the default).
pub fn projects_root(s: &Session) -> Option<PathBuf> {
    let home = s.services.home_dir();
    match &s.prefs.general.projects_folder {
        Some(p) if unsafe_root(p, &homes(s)).is_none() => Some(p.clone()),
        _ => default_folder(home.as_deref()),
    }
}

/// Whether `path` (a project or folder inside `root`) is in, or is, a hidden or support folder
/// (Auto-Save, Proxies, previews…): those aren't renamed or trashed from the Project Manager.
fn protected_entry(root: &Path, path: &Path) -> bool {
    let Ok(rel) = entry_path(path).strip_prefix(canonical(root)).map(Path::to_path_buf) else { return false };
    rel.components().any(|c| match c {
        Component::Normal(n) => {
            let n = n.to_string_lossy();
            n.starts_with('.') || SUPPORT_DIRS.contains(&n.as_ref())
        }
        _ => false,
    })
}

/// Whether the entry `path` itself (not a link's target) is inside `root`.
fn inside_entry(root: &Path, path: &Path) -> bool {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let (r, p) = (canonical(root), entry_path(path));
    p.starts_with(&r) && p != r
}

/// A single name for a project or folder: trimmed, a trailing `.fcproj` dropped; no separators,
/// `.` / `..`, control characters, leading dot or overlong names.
pub fn check_name(name: &str) -> std::result::Result<String, String> {
    let mut n = name.trim().to_string();
    if n.len() > EXT.len() + 1 && n.to_ascii_lowercase().ends_with(&format!(".{EXT}")) {
        n.truncate(n.len() - EXT.len() - 1);
        n = n.trim_end().to_string();
    }
    if n.is_empty() {
        return Err("the name is empty".into());
    }
    if n == "." || n == ".." || n.starts_with('.') {
        return Err(format!("\u{201c}{n}\u{201d} can't be used as a name"));
    }
    if n.chars().any(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()) {
        return Err("a name can't contain / \\ : * ? \" < > | or control characters".into());
    }
    if n.len() > MAX_NAME {
        return Err(format!("the name is longer than {MAX_NAME} bytes"));
    }
    Ok(n)
}

/// `folder` (relative to `root`, `/`-separated) as a path inside `root`; refuses anything that
/// climbs out.
pub fn resolve_folder(root: &Path, folder: &str) -> std::result::Result<PathBuf, String> {
    let mut p = root.to_path_buf();
    for part in folder.split(['/', '\\']).filter(|x| !x.is_empty()) {
        let n = check_name(part).map_err(|e| format!("folder `{folder}`: {e}"))?;
        if n != part {
            return Err(format!("folder `{folder}` is not a folder name"));
        }
        p.push(n);
    }
    Ok(p)
}

/// Whether `path` is inside `root` (both resolved; the root itself counts when `allow_root`).
fn inside(root: &Path, path: &Path, allow_root: bool) -> bool {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let (r, p) = (canonical(root), canonical(path));
    p.starts_with(&r) && (allow_root || p != r)
}

fn relative(root: &Path, path: &Path) -> String {
    canonical(path)
        .strip_prefix(canonical(root))
        .map(|r| {
            r.components()
                .filter_map(|c| if let Component::Normal(n) = c { Some(n.to_string_lossy().into_owned()) } else { None })
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default()
}

fn matches(name: &str, search: &str) -> bool {
    search.is_empty() || name.to_lowercase().contains(&search.to_lowercase())
}

fn sort_by<T>(
    v: &mut [T],
    key: SortKey,
    descending: bool,
    name: impl Fn(&T) -> String,
    modified: impl Fn(&T) -> Option<u64>,
    created: impl Fn(&T) -> Option<u64>,
) {
    v.sort_by(|a, b| {
        let o = match key {
            SortKey::Name => name(a).to_lowercase().cmp(&name(b).to_lowercase()),
            // newest first reads naturally for dates
            SortKey::Modified => modified(b).cmp(&modified(a)),
            SortKey::Created => created(b).cmp(&created(a)),
        };
        let o = o.then_with(|| name(a).cmp(&name(b)));
        if descending { o.reverse() } else { o }
    });
}

/// List `folder` of the projects folder `root`: its sub-folders and projects, plus the `recent`
/// projects outside `root` (on the top level only), filtered by `search` and sorted.
pub fn list(
    root: &Path,
    folder: &str,
    sort: SortKey,
    descending: bool,
    search: &str,
    recent: &[String],
    cache: Option<&Path>,
) -> std::result::Result<Listing, String> {
    let dir = resolve_folder(root, folder)?;
    let mut out = Listing {
        root: root.to_string_lossy().into_owned(),
        folder: folder.split(['/', '\\']).filter(|x| !x.is_empty()).collect::<Vec<_>>().join("/"),
        path: dir.to_string_lossy().into_owned(),
        exists: dir.is_dir(),
        ..Default::default()
    };
    out.breadcrumb = out.folder.split('/').filter(|x| !x.is_empty()).map(str::to_string).collect();
    if out.exists {
        let mut names: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                (!name.starts_with('.')).then(|| (name, e.path()))
            })
            // bounded before collecting: a folder with millions of files can't exhaust memory
            .take(MAX_ENTRIES)
            .collect();
        names.sort();
        for (name, path) in names {
            let Ok(meta) = std::fs::metadata(&path) else {
                // a dangling link or an unreadable entry: only project files are worth showing
                if is_project_file(&path) && matches(&name, search) {
                    out.projects.push(summary(cache, &path));
                }
                continue;
            };
            if meta.is_dir() {
                if SUPPORT_DIRS.contains(&name.as_str()) || !matches(&name, search) {
                    continue;
                }
                let projects = std::fs::read_dir(&path).map(|rd| rd.flatten().take(MAX_ENTRIES).filter(|e| is_project_file(&e.path())).count()).unwrap_or(0);
                let rel = if out.folder.is_empty() { name.clone() } else { format!("{}/{name}", out.folder) };
                out.folders.push(FolderEntry {
                    name,
                    path: path.to_string_lossy().into_owned(),
                    folder: rel,
                    modified: unix(meta.modified()),
                    created: unix(meta.created()),
                    projects,
                });
            } else if is_project_file(&path) && matches(&stem_of(&path), search) {
                out.projects.push(summary(cache, &path));
            }
        }
    }
    if out.folder.is_empty() {
        let mut seen: Vec<PathBuf> = out.projects.iter().map(|p| canonical(Path::new(&p.path))).collect();
        for r in recent.iter().take(64) {
            let p = Path::new(r);
            let c = canonical(p);
            if seen.contains(&c) || (p.exists() && inside(root, p, false)) || !matches(&stem_of(p), search) {
                continue;
            }
            seen.push(c);
            out.other.push(summary(cache, p));
        }
    }
    sort_by(&mut out.folders, sort, descending, |f| f.name.clone(), |f| f.modified, |f| f.created);
    sort_by(&mut out.projects, sort, descending, |p| p.name.clone(), |p| p.modified, |p| p.created);
    Ok(out)
}

/// `base`, `base 2`, `base 3`… in `dir`: the first name with no `<name><ext>` there.
fn free_name(dir: &Path, base: &str, ext: &str) -> Option<String> {
    candidates(base).find(|n| !exists_entry(&dir.join(format!("{n}{ext}"))))
}

fn candidates(base: &str) -> impl Iterator<Item = String> + '_ {
    (1..10_000).map(move |i| if i == 1 { base.to_string() } else { format!("{base} {i}") })
}

fn already_exists(path: &Path) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", path.display()))
}

/// Write `bytes` to the new file `to`, never replacing anything there (not even a dangling
/// link): a temporary file next to it is hard-linked into place, which fails when `to` exists;
/// where hard links aren't supported, a checked rename.
pub fn write_new(to: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = to.parent().ok_or_else(|| std::io::Error::other("no folder"))?;
    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let tmp = dir.join(format!(".{name}.{}-{nanos}.fc-tmp", std::process::id()));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).and_then(|mut f| f.write_all(bytes).and_then(|_| f.sync_all()));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let r = match std::fs::hard_link(&tmp, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(_) if exists_entry(to) => Err(already_exists(to)),
        Err(_) => std::fs::rename(&tmp, to),
    };
    let _ = std::fs::remove_file(&tmp);
    r
}

/// Write `bytes` as `<base><ext>` in `dir`, or `<base> 2<ext>`… when taken (never replacing).
fn write_free(dir: &Path, base: &str, ext: &str, bytes: &[u8]) -> std::io::Result<(String, PathBuf)> {
    for n in candidates(base) {
        let to = dir.join(format!("{n}{ext}"));
        if exists_entry(&to) {
            continue;
        }
        match write_new(&to, bytes) {
            Ok(()) => return Ok((n, to)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free name"))
}

/// Move the entry `from` to `to` without replacing anything at `to`. A file is hard-linked to the
/// new name, then its old name removed (the link fails if `to` exists); a folder or a link is
/// renamed after a check (no atomic no-replace rename without platform calls).
pub fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_file() {
        match std::fs::hard_link(from, to) {
            Ok(()) => {
                if let Err(e) = std::fs::remove_file(from) {
                    let _ = std::fs::remove_file(to);
                    return Err(e);
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
    }
    if exists_entry(to) {
        return Err(already_exists(to));
    }
    std::fs::rename(from, to)
}

// ------------------------------------------------------------------ trash

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYY-MM-DDThh:mm:ss` of a Unix time (+ an offset in seconds).
pub fn iso_local(secs: i64, offset: i32) -> String {
    let t = secs.saturating_add(i64::from(offset));
    let (days, rem) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let (y, mo, d) = civil(days);
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Percent-encode a path for a `.trashinfo` `Path=` line (RFC 2396 escaping; `/` kept).
fn trash_escape(path: &Path) -> String {
    let mut s = String::new();
    for b in path.as_os_str().as_encoded_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => s.push(*b as char),
            b => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

/// Move `path` into the freedesktop.org trash folder `trash` (`…/Trash`, made if needed): a
/// `.trashinfo` file is reserved first (create-new, so two trashings never collide), then the
/// file is renamed into `files/`. Returns where it went. Never copies + deletes.
pub fn trash_into(trash: &Path, path: &Path, now: i64, offset: i32) -> std::io::Result<PathBuf> {
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let files = trash.join("files");
    let info = trash.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;
    let name = abs.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| std::io::Error::other("nothing to trash"))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let body = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", trash_escape(&abs), iso_local(now, offset));
    for i in 1..10_000u32 {
        let cand = if i == 1 { name.clone() } else { format!("{stem}.{i}{ext}") };
        let info_path = info.join(format!("{cand}.trashinfo"));
        let dest = files.join(&cand);
        if exists_entry(&dest) {
            continue;
        }
        let f = std::fs::OpenOptions::new().write(true).create_new(true).open(&info_path);
        let mut f = match f {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        use std::io::Write;
        if let Err(e) = f.write_all(body.as_bytes()).and_then(|_| f.sync_all()) {
            let _ = std::fs::remove_file(&info_path);
            return Err(e);
        }
        drop(f);
        return match move_no_replace(&abs, &dest) {
            Ok(()) => Ok(dest),
            Err(e) => {
                let _ = std::fs::remove_file(&info_path);
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    continue;
                }
                Err(e)
            }
        };
    }
    Err(std::io::Error::other("the trash is full of files with this name"))
}

/// The user's trash folder: `$XDG_DATA_HOME/Trash` (default `~/.local/share/Trash`).
pub fn home_trash(home: Option<&str>) -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|x| !x.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(x.join("Trash"));
    }
    home.filter(|h| !h.is_empty()).map(|h| Path::new(h).join(".local/share/Trash"))
}

/// Move `path` to the operating system's trash. `trash` overrides the trash folder (tests).
pub fn move_to_trash(path: &Path, home: Option<&str>, trash: Option<&Path>, offset: i32) -> std::io::Result<PathBuf> {
    let now = unix(Ok(SystemTime::now())).unwrap_or(0) as i64;
    if let Some(t) = trash {
        return trash_into(t, path, now, offset);
    }
    if cfg!(target_os = "macos") {
        // the Finder's trash (no "Put Back" record, but nothing is deleted)
        let t = home.map(|h| Path::new(h).join(".Trash")).ok_or_else(|| std::io::Error::other("no home folder"))?;
        let name = path.file_name().ok_or_else(|| std::io::Error::other("nothing to trash"))?.to_string_lossy().into_owned();
        let dest = free_name(&t, &name, "").map(|n| t.join(n)).ok_or_else(|| std::io::Error::other("the trash is full"))?;
        move_no_replace(path, &dest)?;
        return Ok(dest);
    }
    if cfg!(any(windows, target_arch = "wasm32")) {
        return Err(std::io::Error::other("moving to the Recycle Bin isn't available on this system yet; nothing was deleted"));
    }
    let t = home_trash(home).ok_or_else(|| std::io::Error::other("no trash folder (no home folder)"))?;
    match trash_into(&t, path, now, offset) {
        Ok(p) => Ok(p),
        // another file system: its own `.Trash-<uid>` at the top of that file system
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            let top = topdir_trash(path, home)?;
            trash_into(&top, path, now, offset)
        }
        Err(e) => Err(e),
    }
}

/// `$topdir/.Trash-$uid` of the file system holding `path` (freedesktop.org trash spec), checked
/// by [`checked_trash_dir`].
#[cfg(unix)]
fn topdir_trash(path: &Path, home: Option<&str>) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata(home.ok_or_else(|| std::io::Error::other("no home folder"))?)?.uid();
    let abs = entry_path(path);
    let dev = std::fs::symlink_metadata(&abs)?.dev();
    let mut top = abs.parent().ok_or_else(|| std::io::Error::other("no folder"))?.to_path_buf();
    while let Some(up) = top.parent() {
        if std::fs::metadata(up)?.dev() != dev {
            break;
        }
        top = up.to_path_buf();
    }
    checked_trash_dir(&top.join(format!(".Trash-{uid}")), uid)
}
#[cfg(not(unix))]
fn topdir_trash(_: &Path, _: Option<&str>) -> std::io::Result<PathBuf> {
    Err(std::io::Error::other("no trash folder on that drive"))
}

/// A per-user trash folder at the top of a file system, made (mode 0700) when missing. Anyone
/// can plant one there, so it must be a real folder (not a link), owned by `uid`, mode 0700;
/// otherwise nothing is moved.
#[cfg(unix)]
pub fn checked_trash_dir(t: &Path, uid: u32) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::symlink_metadata(t) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new().mode(0o700).create(t)?,
        Err(e) => return Err(e),
    }
    let m = std::fs::symlink_metadata(t)?;
    if !m.file_type().is_dir() || m.uid() != uid || m.mode() & 0o7777 != 0o700 {
        return Err(std::io::Error::other(format!(
            "{} is not a safe trash folder (it must be a real folder you own with mode 700); nothing was moved",
            t.display()
        )));
    }
    Ok(t.to_path_buf())
}

// ------------------------------------------------------------------ commands

fn other(e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(e.to_string())
}

fn root_of(s: &Session, cmd: &str) -> Result<PathBuf> {
    projects_root(s).ok_or_else(|| bad(cmd, "no projects folder (set Settings ▸ General ▸ Projects folder)"))
}

/// The projects folder, made on first use.
fn ensure_root(s: &Session, cmd: &str) -> Result<PathBuf> {
    let r = root_of(s, cmd)?;
    std::fs::create_dir_all(&r).map_err(|e| other(format!("{}: {e}", r.display())))?;
    Ok(r)
}

fn folder_of(s: &Session, cmd: &str, p: &Value) -> Result<PathBuf> {
    let root = root_of(s, cmd)?;
    resolve_folder(&root, str_p(p, "folder").unwrap_or("")).map_err(|e| bad(cmd, e))
}

fn path_p(cmd: &str, p: &Value) -> Result<PathBuf> {
    let path = str_p(p, "path").filter(|x| !x.is_empty()).ok_or_else(|| bad(cmd, "need `path`"))?;
    Ok(PathBuf::from(path))
}

fn same_file(a: &Path, b: &Path) -> bool {
    canonical(a) == canonical(b)
}

fn is_open(s: &Session, path: &Path) -> bool {
    s.path.as_deref().is_some_and(|o| same_file(Path::new(o), path))
}

/// Replace (or drop, `to` = None) a path in the recent projects.
fn update_recents(s: &mut Session, from: &Path, to: Option<&Path>) {
    let mut next = s.prefs.clone();
    let before = next.general.recent_projects.clone();
    let mut out: Vec<String> = Vec::new();
    for r in &before {
        if same_file(Path::new(r), from) || Path::new(r) == from {
            if let Some(t) = to {
                out.push(t.to_string_lossy().into_owned());
            }
        } else {
            out.push(r.clone());
        }
    }
    out.dedup();
    if out != before {
        next.general.recent_projects = out;
        let _ = s.set_prefs(next);
    }
}

/// After a project or folder moved from `from` (as given, and resolved before the move) to `to`
/// (None: to the trash): rewrite the recent projects at or under it.
fn rewrite_recents(s: &mut Session, from: &[&Path], to: Option<&Path>) {
    let mut next = s.prefs.clone();
    let before = next.general.recent_projects.clone();
    let mut out: Vec<String> = Vec::new();
    for r in &before {
        let rest = from.iter().find_map(|f| Path::new(r).strip_prefix(f).ok());
        match (rest, to) {
            (None, _) => out.push(r.clone()),
            (Some(rest), Some(t)) => out.push(if rest.as_os_str().is_empty() { t.to_path_buf() } else { t.join(rest) }.to_string_lossy().into_owned()),
            (Some(_), None) => {}
        }
    }
    out.dedup();
    if out != before {
        next.general.recent_projects = out;
        let _ = s.set_prefs(next);
    }
}

/// Most cache entries looked at when a project or folder moves.
const MAX_CACHE_SCAN: usize = 20_000;

/// After a project or folder moved from `from` (resolved, before the move) to `to` (None: to the
/// trash): carry its cached summaries and posters to the new paths, or drop them.
fn move_cache(cache: Option<&Path>, from: &Path, to: Option<&Path>) {
    let Some(cache) = cache else { return };
    let Ok(rd) = std::fs::read_dir(cache) else { return };
    let jsons: Vec<PathBuf> = rd.flatten().take(MAX_CACHE_SCAN).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    for json_path in jsons {
        let Some(key) = json_path.file_stem().map(|k| k.to_string_lossy().into_owned()) else { continue };
        let Some(mut e) = read_entry(cache, &key) else { continue };
        let Ok(rest) = Path::new(&e.path).strip_prefix(from).map(Path::to_path_buf) else { continue };
        let png = cache.join(format!("{key}.png"));
        match to {
            Some(t) => {
                let new_path = if rest.as_os_str().is_empty() { t.to_path_buf() } else { t.join(&rest) };
                let new_key = cache_key(&new_path);
                if new_key == key {
                    continue;
                }
                e.path = canonical(&new_path).to_string_lossy().into_owned();
                e.summary.path = new_path.to_string_lossy().into_owned();
                e.summary.name = stem_of(&new_path);
                if write_entry(cache, &new_key, &e).is_ok() {
                    let _ = std::fs::remove_file(&json_path);
                    if png.is_file() {
                        let _ = std::fs::rename(&png, cache.join(format!("{new_key}.png")));
                    }
                }
            }
            None => {
                // (cache files only: the project itself went to the trash)
                let _ = std::fs::remove_file(&json_path);
                let _ = std::fs::remove_file(&png);
            }
        }
    }
}

/// What a rename / trash acts on: the entry itself (a link, never its target).
struct Target {
    /// A real folder (not a link).
    dir: bool,
    link: bool,
}

fn target(s: &Session, cmd: &str, path: &Path) -> Result<Target> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| other(format!("{}: {e}", path.display())))?;
    let root = root_of(s, cmd).ok();
    let in_root = root.as_deref().is_some_and(|r| inside_entry(r, path));
    if in_root && root.as_deref().is_some_and(|r| protected_entry(r, path)) {
        return Err(bad(cmd, "hidden folders and Revolution's own folders (Auto-Save, Proxies, previews) can't be changed here"));
    }
    let link = meta.file_type().is_symlink();
    let dir = meta.is_dir();
    // a folder, or a link that isn't a project file: only inside the projects folder
    if (dir || (link && !is_project_file(path))) && !in_root {
        return Err(bad(cmd, "only folders inside the projects folder can be changed here"));
    }
    if !dir && !link && !is_project_file(path) {
        return Err(bad(cmd, "not a project file"));
    }
    if dir && s.path.as_deref().is_some_and(|o| inside(path, Path::new(o), false)) {
        return Err(bad(cmd, "the open project is in this folder; open another project first"));
    }
    if !dir && (is_open(s, path) || s.path.as_deref().is_some_and(|o| Path::new(o) == path)) {
        return Err(bad(cmd, "this project is open; open another project first"));
    }
    if link && s.path.as_deref().is_some_and(|o| Path::new(o).starts_with(path)) {
        return Err(bad(cmd, "the open project is reached through this link; open another project first"));
    }
    Ok(Target { dir, link })
}

fn list_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.list";
    let root = root_of(s, CMD)?;
    let sort = match str_p(p, "sort") {
        Some(x) => SortKey::parse(x).ok_or_else(|| bad(CMD, format!("unknown sort `{x}` (name, modified, created)")))?,
        None => SortKey::Name,
    };
    let cache = cache_dir(s);
    let l = list(
        &root,
        str_p(p, "folder").unwrap_or(""),
        sort,
        bool_p(p, "descending").unwrap_or(false),
        str_p(p, "search").unwrap_or(""),
        &s.prefs.general.recent_projects,
        cache.as_deref(),
    )
    .map_err(|e| bad(CMD, e))?;
    let mut l = l;
    l.warning = root_warning(s);
    if let Some(w) = &l.warning {
        log::warn!("{w}");
    }
    serde_json::to_value(l).map_err(other)
}

fn open_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.open";
    let path = path_p(CMD, p)?;
    if !path.is_file() {
        return Err(bad(CMD, format!("{} is missing", path.display())));
    }
    if s.is_dirty() && !bool_p(p, "discard").unwrap_or(false) {
        return Err(bad(CMD, "the open project has unsaved changes (save it, or pass `discard`: true)"));
    }
    let mut r = s.execute("file.open", json!({"path": path.to_string_lossy()}))?;
    let page = summary(cache_dir(s).as_deref(), &path).page;
    r["page"] = json!(page);
    Ok(r)
}

fn new_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.new";
    let name = check_name(str_p(p, "name").unwrap_or("")).map_err(|e| bad(CMD, e))?;
    if s.is_dirty() && !bool_p(p, "discard").unwrap_or(false) {
        return Err(bad(CMD, "the open project has unsaved changes (save it, or pass `discard`: true)"));
    }
    ensure_root(s, CMD)?;
    let dir = folder_of(s, CMD, p)?;
    std::fs::create_dir_all(&dir).map_err(|e| other(format!("{}: {e}", dir.display())))?;
    let path = dir.join(format!("{name}.{EXT}"));
    if exists_entry(&path) {
        return Err(bad(CMD, format!("a project named \u{201c}{name}\u{201d} already exists here")));
    }
    s.execute("file.newProject", json!({"name": name}))?;
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()}))?;
    Ok(json!({"path": path.to_string_lossy(), "name": name}))
}

fn rename_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.rename";
    let path = path_p(CMD, p)?;
    let name = check_name(str_p(p, "name").unwrap_or("")).map_err(|e| bad(CMD, e))?;
    let dir = path.parent().ok_or_else(|| bad(CMD, "no folder"))?.to_path_buf();
    let t = target(s, CMD, &path)?;
    let to = if is_project_file(&path) && !t.dir { dir.join(format!("{name}.{EXT}")) } else { dir.join(&name) };
    let resolved = entry_path(&path);
    if to == path {
        return Ok(json!({"path": to.to_string_lossy(), "name": name}));
    }
    // (a case-only rename on a case-insensitive disk is the same entry)
    let same = exists_entry(&to) && entry_path(&to) == resolved || (!t.link && same_file(&to, &path));
    let moved = if same { std::fs::rename(&path, &to) } else { move_no_replace(&path, &to) };
    moved.map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => bad(CMD, format!("\u{201c}{name}\u{201d} already exists here")),
        _ => other(format!("{}: {e}", path.display())),
    })?;
    rewrite_recents(s, &[&path, &resolved], Some(&to));
    if !t.link {
        move_cache(cache_dir(s).as_deref(), &resolved, Some(&entry_path(&to)));
    }
    Ok(json!({"path": to.to_string_lossy(), "name": name}))
}

fn duplicate_cmd(_: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.duplicate";
    let path = path_p(CMD, p)?;
    if !is_project_file(&path) || !path.is_file() {
        return Err(bad(CMD, "not a project file"));
    }
    let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if len > MAX_PROJECT_BYTES {
        return Err(bad(CMD, format!("the file is larger than {} MiB", MAX_PROJECT_BYTES >> 20)));
    }
    let dir = path.parent().ok_or_else(|| bad(CMD, "no folder"))?;
    let bytes = read_capped(&path, MAX_PROJECT_BYTES).ok_or_else(|| other(format!("{}: can't read", path.display())))?;
    let (name, to) = match str_p(p, "name") {
        Some(n) => {
            let n = check_name(n).map_err(|e| bad(CMD, e))?;
            let to = dir.join(format!("{n}.{EXT}"));
            write_new(&to, &bytes).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => bad(CMD, format!("\u{201c}{n}\u{201d} already exists here")),
                _ => other(format!("{}: {e}", to.display())),
            })?;
            (n, to)
        }
        None => write_free(dir, &format!("{} copy", stem_of(&path)), &format!(".{EXT}"), &bytes).map_err(|e| other(format!("{}: {e}", dir.display())))?,
    };
    Ok(json!({"path": to.to_string_lossy(), "name": name}))
}

fn new_folder_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.newFolder";
    let name = check_name(str_p(p, "name").unwrap_or("")).map_err(|e| bad(CMD, e))?;
    ensure_root(s, CMD)?;
    let parent = folder_of(s, CMD, p)?;
    std::fs::create_dir_all(&parent).map_err(|e| other(format!("{}: {e}", parent.display())))?;
    let dir = parent.join(&name);
    // create (not create_all): fails, rather than reuses, when anything is there
    std::fs::create_dir(&dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => bad(CMD, format!("\u{201c}{name}\u{201d} already exists here")),
        _ => other(format!("{}: {e}", dir.display())),
    })?;
    let root = root_of(s, CMD)?;
    Ok(json!({"path": dir.to_string_lossy(), "folder": relative(&root, &dir)}))
}

fn trash_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.trash";
    let path = path_p(CMD, p)?;
    let t = target(s, CMD, &path)?;
    let resolved = entry_path(&path);
    let home = s.services.home_dir();
    let offset = s.persistence.as_ref().map_or(0, |per| (per.local_offset)(unix(Ok(SystemTime::now())).unwrap_or(0) as i64));
    let trash = s.trash_dir.clone();
    let to = move_to_trash(&path, home.as_deref(), trash.as_deref(), offset).map_err(|e| other(format!("{}: {e}", path.display())))?;
    if !t.link {
        // a link went to the trash, its target didn't: its recents and cache stay
        rewrite_recents(s, &[&path, &resolved], None);
        move_cache(cache_dir(s).as_deref(), &resolved, None);
    }
    Ok(json!({"path": path.to_string_lossy(), "trashed": to.to_string_lossy()}))
}

fn export_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.export";
    let path = path_p(CMD, p)?;
    let dest = str_p(p, "destination").filter(|d| !d.is_empty()).ok_or_else(|| bad(CMD, "need `destination` (a folder)"))?;
    if !is_project_file(&path) || !path.is_file() {
        return Err(bad(CMD, "not a project file"));
    }
    // the open project as it is now (saved or not), else the file
    let mut tmp = Session::new(s.services.clone());
    tmp.prefs.timeline.restore_open_sequences = false;
    // this computer's data folder (its user shader effects go into packages, S1 item 5d)
    tmp.prefs_path = s.prefs_path.clone();
    if !is_open(s, &path) {
        tmp.execute("file.open", json!({"path": path.to_string_lossy()}))?;
    } else {
        tmp.project = s.project.clone();
        tmp.path = s.path.clone();
    }
    let name = stem_of(&path);
    if tmp.project.sequences().next().is_none() {
        // nothing to collect: the archive is the project file
        let dir = Path::new(dest);
        std::fs::create_dir_all(dir).map_err(|e| other(format!("{dest}: {e}")))?;
        let to = dir.join(format!("{name}.{EXT}"));
        let bytes = filmcraft_format::encode(&tmp.project, true);
        write_new(&to, &bytes).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => bad(CMD, format!("{} already exists", to.display())),
            _ => other(format!("{}: {e}", to.display())),
        })?;
        return Ok(json!({"project": to.to_string_lossy(), "job": Value::Null}));
    }
    let wait = bool_p(p, "wait").unwrap_or(false);
    let mut r = tmp.execute(
        "file.projectManager",
        json!({"destination": dest, "mode": "collect", "excludeUnused": false, "includeProxies": false, "projectName": name, "wait": wait}),
    )?;
    // the copy runs on: its job joins this session's (status bar, `jobs.list`)
    if let Some(mut job) = tmp.jobs.pop() {
        job.id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        r["job"] = json!(job.id);
        s.jobs.push(job);
    }
    Ok(r)
}

/// `projects.package` (STORAGE-PLAN S1 item 5): `<destination>/<name> package/` with the project,
/// every media file it uses (and its proxies with `proxies`), and a `manifest.json` listing each
/// file's size and SHA-256 so the receiver can check it. Runs as a background job (`wait` to block);
/// a failed or stopped package is removed again.
fn package_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.package";
    let path = path_p(CMD, p)?;
    let dest = str_p(p, "destination").filter(|d| !d.is_empty()).ok_or_else(|| bad(CMD, "need `destination` (a folder)"))?;
    if !is_project_file(&path) || !path.is_file() {
        return Err(bad(CMD, "not a project file"));
    }
    let mut tmp = Session::new(s.services.clone());
    tmp.prefs.timeline.restore_open_sequences = false;
    // this computer's data folder (its user shader effects go into packages, S1 item 5d)
    tmp.prefs_path = s.prefs_path.clone();
    if !is_open(s, &path) {
        tmp.execute("file.open", json!({"path": path.to_string_lossy()}))?;
    } else {
        tmp.project = s.project.clone();
        tmp.path = s.path.clone();
    }
    if tmp.project.sequences().next().is_none() {
        return Err(bad(CMD, "the project has no sequences to package"));
    }
    let name = stem_of(&path);
    let folder = Path::new(dest).join(format!("{name} package"));
    let dry = bool_p(p, "dryRun").unwrap_or(false);
    if folder.exists() && !dry {
        return Err(bad(CMD, format!("{} already exists", folder.display())));
    }
    // `format: "file"`: one `<name>.fcpackage` (the folder is made, zipped and removed)
    let file = match str_p(p, "format").unwrap_or("folder") {
        "folder" => None,
        "file" => Some(Path::new(dest).join(format!("{name}.{}", crate::package::EXT))),
        f => return Err(bad(CMD, format!("unknown format `{f}` (folder | file)"))),
    };
    if let Some(f) = &file
        && f.exists()
        && !dry
    {
        return Err(bad(CMD, format!("{} already exists", f.display())));
    }
    let mut r = tmp.execute(
        "file.projectManager",
        json!({
            "destination": folder.to_string_lossy(),
            "mode": "collect",
            "excludeUnused": false,
            "includeProxies": bool_p(p, "proxies").unwrap_or(false),
            "projectName": name,
            "manifest": true,
            // S1 item 5d: MP4 / MOV cut to the used parts by stream copy (never re-encoded)
            "trim": bool_p(p, "trim").unwrap_or(false),
            "handles": u64_p(p, "handles").unwrap_or(30),
            "zipTo": file.as_ref().map(|f| f.to_string_lossy().into_owned()),
            "dryRun": dry,
            "wait": bool_p(p, "wait").unwrap_or(false),
        }),
    )?;
    r["package"] = json!(file.as_ref().unwrap_or(&folder).to_string_lossy());
    if dry {
        // the size estimate: the bytes that would be copied
        r["estimateBytes"] = r["resultBytes"].clone();
        return Ok(r);
    }
    if let Some(mut job) = tmp.jobs.pop() {
        job.id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
        r["job"] = json!(job.id);
        s.jobs.push(job);
    }
    Ok(r)
}

/// Copy a folder (bounded depth, links skipped), counting into `prog`.
fn copy_tree(from: &Path, to: &Path, depth: u32, prog: &filmcraft_export::Progress) -> std::io::Result<u64> {
    use std::sync::atomic::Ordering;
    if depth > 32 {
        return Err(std::io::Error::other("folders nested too deep"));
    }
    std::fs::create_dir_all(to)?;
    let mut n = 0;
    for e in std::fs::read_dir(from)? {
        if prog.cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        let e = e?;
        let ty = e.file_type()?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            n += copy_tree(&src, &dst, depth + 1, prog)?;
        } else if ty.is_file() {
            n += std::fs::copy(&src, &dst)?;
            prog.done.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(n)
}

fn count_files(dir: &Path, depth: u32) -> u64 {
    if depth > 32 {
        return 0;
    }
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => count_files(&e.path(), depth + 1),
                    Ok(t) if t.is_file() => 1,
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

fn import_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.import";
    let path = path_p(CMD, p)?;
    ensure_root(s, CMD)?;
    let dir = folder_of(s, CMD, p)?;
    std::fs::create_dir_all(&dir).map_err(|e| other(format!("{}: {e}", dir.display())))?;
    let meta = std::fs::metadata(&path).map_err(|e| other(format!("{}: {e}", path.display())))?;
    if meta.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(crate::package::EXT)) {
        let m = crate::package::read_zip_manifest(&path).map_err(|e| bad(CMD, e))?;
        return import_package_file(s, p, &path, &dir, m);
    }
    if meta.is_file() {
        if !is_project_file(&path) {
            return Err(bad(CMD, "choose a .fcproj project file or an exported project folder"));
        }
        let bytes = read_capped(&path, MAX_PROJECT_BYTES).ok_or_else(|| bad(CMD, format!("{} can't be read (or is larger than 64 MiB)", path.display())))?;
        // only real projects come in
        decode_guarded(&bytes).map_err(|e| bad(CMD, format!("{}: {e}", path.display())))?;
        let (_, to) = write_free(&dir, &stem_of(&path), &format!(".{EXT}"), &bytes).map_err(|e| other(format!("{}: {e}", dir.display())))?;
        return Ok(json!({"path": to.to_string_lossy(), "job": Value::Null}));
    }
    if !meta.is_dir() {
        return Err(bad(CMD, "nothing to import"));
    }
    let root = root_of(s, CMD)?;
    if inside(&path, &root, true) || inside(&root, &path, true) {
        return Err(bad(CMD, "the folder is (or holds) the projects folder"));
    }
    let has_project = std::fs::read_dir(&path).map(|rd| rd.flatten().take(MAX_ENTRIES).any(|e| is_project_file(&e.path()))).unwrap_or(false);
    if !has_project {
        return Err(bad(CMD, "the folder has no .fcproj project in it"));
    }
    // a package (`projects.package`): checked against its manifest, then relinked to the copies
    let manifest_path = path.join(crate::package::MANIFEST_FILE);
    let manifest = if manifest_path.is_file() {
        let parsed = read_capped(&manifest_path, crate::package::MAX_MANIFEST_BYTES)
            .ok_or_else(|| "the package manifest can't be read".to_string())
            .and_then(|b| crate::package::Manifest::parse(&b));
        match parsed {
            Ok(m) => Some(m),
            Err(e)
                if bool_p(p, "package").unwrap_or(false)
                    || read_capped(&manifest_path, 4096).is_some_and(|b| String::from_utf8_lossy(&b).contains(crate::package::FORMAT)) =>
            {
                return Err(bad(CMD, e));
            }
            // somebody else's manifest.json: an ordinary folder
            Err(_) => None,
        }
    } else if bool_p(p, "package").unwrap_or(false) {
        return Err(bad(CMD, "this folder isn't a Revolution package (no manifest.json)"));
    } else {
        None
    };
    let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported".into());
    // reserve the destination folder (create, not create_all: never merge into one that appeared)
    let mut reserved = None;
    for n in candidates(&base) {
        let to = dir.join(&n);
        match std::fs::create_dir(&to) {
            Ok(()) => {
                reserved = Some((n, to));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(other(format!("{}: {e}", to.display()))),
        }
    }
    let (name, to) = reserved.ok_or_else(|| bad(CMD, "no free name"))?;
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: format!("Import project: {name}"), progress: Default::default(), result: Default::default() };
    job.progress.total.store(count_files(&path, 0).max(1), std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let (from, dst) = (path.clone(), to.clone());
    let is_package = manifest.is_some();
    let effects = crate::user_effects::folder(s);
    let run = move || {
        let t0 = web_time::Instant::now();
        let r = match &manifest {
            Some(m) => import_package(&from, &dst, m, effects.as_deref(), &prog),
            None => copy_tree(&from, &dst, 0, &prog).map_err(|e| e.to_string()),
        };
        if r.is_err() && manifest.is_some() {
            // a package that didn't check out or didn't copy leaves nothing behind
            let _ = std::fs::remove_dir_all(&dst);
        }
        let r = r.map(|bytes| filmcraft_export::Report {
            path: dst.to_string_lossy().into_owned(),
            frames: prog.done.load(std::sync::atomic::Ordering::Relaxed),
            seconds: t0.elapsed().as_secs_f64(),
            bytes,
            render_fps: 0.0,
            extra_files: Vec::new(),
        });
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
        }
        prog.finished.store(true, std::sync::atomic::Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-project-import".into()).spawn(run).map_err(other)?;
    }
    Ok(json!({"path": to.to_string_lossy(), "job": id, "package": is_package}))
}

/// A package comes in: checked against its manifest (every file there, the right size and
/// checksum), copied, and its project's media and proxies pointed at the copies.
fn import_package(
    from: &Path,
    to: &Path,
    m: &crate::package::Manifest,
    effects: Option<&Path>,
    prog: &filmcraft_export::Progress,
) -> std::result::Result<u64, String> {
    // copied first and the copy checked: the files checked are the files kept, even if the
    // source (a USB disk, a synced share) changes meanwhile; links are neither copied nor followed
    *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = "Copying".into();
    let bytes = copy_tree(from, to, 0, prog).map_err(|e| e.to_string())?;
    *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = "Checking the package".into();
    check_and_relink(to, m, effects, prog)?;
    Ok(bytes)
}

/// Checks an unpacked package against its manifest and relinks its project to the copies.
fn check_and_relink(to: &Path, m: &crate::package::Manifest, effects: Option<&Path>, prog: &filmcraft_export::Progress) -> std::result::Result<(), String> {
    use std::sync::atomic::Ordering;
    let report = crate::package::verify(to, m, &|| prog.cancel.load(Ordering::Relaxed))?;
    if !report.is_complete() {
        let mut why = Vec::new();
        for (what, list) in [("missing", &report.missing), ("the wrong size", &report.wrong_size), ("damaged", &report.damaged)] {
            if !list.is_empty() {
                why.push(format!("{what}: {}", list.iter().take(10).cloned().collect::<Vec<_>>().join(", ")));
            }
        }
        return Err(format!("the package is incomplete ({})", why.join("; ")));
    }
    relink_package(to, m)?;
    if let Some(dir) = effects {
        install_effects(to, m, dir);
    }
    Ok(())
}

/// The package's user shader effects (`Effects/*.wgsl`, S1 item 5d) installed into this
/// computer's Effects folder `dir`: a file that isn't there is copied; one that is there is never
/// overwritten (the same file is fine; a different one keeps this computer's, and is logged).
/// Returns how many were installed. Failures only log: the project still opens, with the effect
/// shown as missing, as before packages carried effects.
pub(crate) fn install_effects(root: &Path, m: &crate::package::Manifest, dir: &Path) -> usize {
    let mut installed = 0;
    for e in m.entries.iter().filter(|e| e.role == crate::package::Role::Shader) {
        let Some(rel) = crate::package::safe_relative(&e.path) else { continue };
        let (Some(name), Some(parent)) = (rel.file_name(), rel.parent()) else { continue };
        if parent != Path::new(crate::project_manager::EFFECTS_FOLDER) {
            continue;
        }
        let (from, to) = (root.join(&rel), dir.join(name));
        match std::fs::symlink_metadata(&to) {
            Ok(_) => {
                if read_capped(&to, filmcraft_usershader::MAX_SOURCE as u64) != read_capped(&from, filmcraft_usershader::MAX_SOURCE as u64) {
                    log::info!("{}: this computer has a different effect of that name; kept it", to.display());
                }
            }
            Err(_) => {
                let ok = std::fs::create_dir_all(dir).and_then(|()| std::fs::copy(&from, &to));
                match ok {
                    Ok(_) => installed += 1,
                    Err(err) => log::warn!("installing {}: {err}", to.display()),
                }
            }
        }
    }
    installed
}

/// Points the unpacked project's file media and proxies at the package's copies: a path whose
/// end matches a manifest entry (the sender's folder is never written into the manifest, so the
/// match is by the path inside the package). Returns how many were relinked.
pub(crate) fn relink_package(root: &Path, m: &crate::package::Manifest) -> std::result::Result<usize, String> {
    let Some(rel) = crate::package::safe_relative(&m.project) else { return Err("the package names no project".into()) };
    let project = root.join(rel);
    let bytes = read_capped(&project, MAX_PROJECT_BYTES).ok_or_else(|| format!("{} can't be read", project.display()))?;
    // a project saved by a newer FilmCraft: the package is still imported (checked, complete) but
    // can't be relinked here; opening it says to update FilmCraft (STORAGE-PLAN S1 item 5: warn,
    // don't fail)
    if let Ok(Err(filmcraft_format::FormatError::TooNew { found, supported })) = std::panic::catch_unwind(|| filmcraft_format::decode(&bytes).map(|_| ())) {
        log::warn!("{}: project schema v{found} is newer than this build (v{supported}); imported without relinking", project.display());
        return Ok(0);
    }
    let loaded = decode_guarded(&bytes)?;
    let mut proj = loaded.project;
    let entries: std::collections::HashSet<&str> = m.entries.iter().filter(|e| e.role != crate::package::Role::Project).map(|e| e.path.as_str()).collect();
    // the sender's package folder: the folder most media paths sit in, below which their rest is
    // an entry (so a path that merely ends like an entry elsewhere on the sender's disk isn't taken)
    let split = |path: &str| -> Option<(String, String)> {
        let path = path.replace('\\', "/");
        let mut at = 0usize;
        while let Some(i) = path.get(at..).and_then(|r| r.find('/')).map(|i| i.saturating_add(at)) {
            let (prefix, rest) = (path.get(..i)?, path.get(i.saturating_add(1)..)?);
            if entries.contains(rest) {
                return Some((prefix.to_string(), rest.to_string()));
            }
            at = i.saturating_add(1);
        }
        None
    };
    let mut roots: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for item in proj.items.values() {
        if let Some(filmcraft_project::MediaRef::File { path }) = item.as_media().map(|m| &m.media)
            && let Some((prefix, _)) = split(path)
        {
            *roots.entry(prefix).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = roots.into_iter().collect();
    ranked.sort_by_key(|r| std::cmp::Reverse(r.1));
    // a tie is ambiguous: relink nothing rather than guess (Link Media still works)
    let sender = match (ranked.first(), ranked.get(1)) {
        (Some(a), Some(b)) if a.1 == b.1 => None,
        (Some(a), _) => Some(a.0.clone()),
        _ => None,
    };
    let find = |path: &str| -> Option<String> {
        let (prefix, rest) = split(path)?;
        (Some(&prefix) == sender.as_ref()).then(|| crate::package::safe_relative(&rest)).flatten().map(|r| root.join(r).to_string_lossy().into_owned())
    };
    let mut n = 0usize;
    for item in proj.items.values_mut() {
        let Some(mc) = item.as_media_mut() else { continue };
        if let filmcraft_project::MediaRef::File { path } = &mut mc.media
            && let Some(new) = find(path)
        {
            *path = new;
            mc.offline = false;
            n = n.saturating_add(1);
        }
        if let Some(filmcraft_project::MediaRef::File { path }) = &mut mc.proxy
            && let Some(new) = find(path)
        {
            *path = new;
        }
    }
    let out = filmcraft_format::encode_with_view(&proj, loaded.view.as_ref(), false);
    filmcraft_format::atomic_write(&project, &out).map_err(|e| format!("{}: {e}", project.display()))?;
    Ok(n)
}

/// A `.fcpackage` file comes in: unpacked into a new folder of the projects list (only the
/// files its manifest lists), checked, and relinked; on any failure the folder is removed.
fn import_package_file(s: &mut Session, p: &Value, file: &Path, dir: &Path, m: crate::package::Manifest) -> Result<Value> {
    const CMD: &str = "projects.import";
    let base = file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported".into());
    let mut reserved = None;
    for n in candidates(&base) {
        let to = dir.join(&n);
        match std::fs::create_dir(&to) {
            Ok(()) => {
                reserved = Some((n, to));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(other(format!("{}: {e}", to.display()))),
        }
    }
    let (name, to) = reserved.ok_or_else(|| bad(CMD, "no free name"))?;
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = crate::Job { id, label: format!("Import project: {name}"), progress: Default::default(), result: Default::default() };
    job.progress.total.store(m.entries.len().max(1) as u64, std::sync::atomic::Ordering::Relaxed);
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let (from, dst) = (file.to_path_buf(), to.clone());
    let effects = crate::user_effects::folder(s);
    let run = move || {
        use std::sync::atomic::Ordering;
        let t0 = web_time::Instant::now();
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = "Unpacking".into();
        let r = crate::package::unzip_package(&from, &dst, &m, &|| prog.cancel.load(Ordering::Relaxed)).and_then(|bytes| {
            *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = "Checking the package".into();
            check_and_relink(&dst, &m, effects.as_deref(), &prog).map(|()| bytes)
        });
        if r.is_err() {
            let _ = std::fs::remove_dir_all(&dst);
        }
        let r = r.map(|bytes| filmcraft_export::Report {
            path: dst.to_string_lossy().into_owned(),
            frames: prog.done.load(Ordering::Relaxed),
            seconds: t0.elapsed().as_secs_f64(),
            bytes,
            render_fps: 0.0,
            extra_files: Vec::new(),
        });
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
        }
        prog.finished.store(true, Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if bool_p(p, "wait").unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-project-import".into()).spawn(run).map_err(other)?;
    }
    Ok(json!({"path": to.to_string_lossy(), "job": id, "package": true}))
}

fn import_package_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let mut q = p.clone();
    q["package"] = json!(true);
    import_cmd(s, &q)
}

fn reveal_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let path = match str_p(p, "path").filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => folder_of(s, "projects.reveal", p)?,
    };
    if !path.exists() {
        return Err(bad("projects.reveal", format!("{} is missing", path.display())));
    }
    Ok(json!({"path": path.to_string_lossy(), "reveal": true}))
}

fn remove_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_p("projects.removeFromList", p)?;
    let before = s.prefs.general.recent_projects.len();
    update_recents(s, &path, None);
    Ok(json!({"removed": before.saturating_sub(s.prefs.general.recent_projects.len())}))
}

fn locate_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "projects.locate";
    let path = path_p(CMD, p)?;
    let to = str_p(p, "newPath").filter(|x| !x.is_empty()).map(PathBuf::from).ok_or_else(|| bad(CMD, "need `newPath`"))?;
    if !is_project_file(&to) || !to.is_file() {
        return Err(bad(CMD, format!("{} is not a project file", to.display())));
    }
    update_recents(s, &path, Some(&to));
    Ok(json!({"path": to.to_string_lossy()}))
}

/// Whether a project is open (something to return to when the home screen closes): saved,
/// edited, or holding anything.
pub fn project_open(s: &Session) -> bool {
    s.path.is_some() || s.is_dirty() || !s.project.items.is_empty()
}

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    params: &'static str,
    run: fn(&mut Session, &Value) -> Result<Value>,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled: always, run, journal }
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        // the frontend opens the home screen; headless it just answers what it would list
        spec("projects.show", "Projects…", &["File"], "{}", |s, _| list_cmd(s, &json!({})), false),
        spec("projects.list", "List Projects", &[], r#"{"folder":str?,"sort":"name|modified|created"?,"descending":bool?,"search":str?}"#, list_cmd, false),
        spec("projects.open", "Open Project", &[], r#"{"path":str,"discard":bool?}"#, open_cmd, true),
        spec("projects.new", "New Project", &[], r#"{"name":str,"folder":str?,"discard":bool?}"#, new_cmd, true),
        spec("projects.rename", "Rename Project", &[], r#"{"path":str,"name":str}"#, rename_cmd, true),
        spec("projects.duplicate", "Duplicate Project", &[], r#"{"path":str,"name":str?}"#, duplicate_cmd, true),
        spec("projects.newFolder", "New Project Folder", &[], r#"{"name":str,"folder":str?}"#, new_folder_cmd, true),
        spec("projects.reveal", "Reveal Project in File Manager", &[], r#"{"path":str?,"folder":str?}"#, reveal_cmd, false),
        spec("projects.removeFromList", "Remove Project from List", &[], r#"{"path":str}"#, remove_cmd, true),
        spec("projects.locate", "Locate Project", &[], r#"{"path":str,"newPath":str}"#, locate_cmd, true),
        spec("projects.trash", "Move Project to Trash", &[], r#"{"path":str}"#, trash_cmd, true),
        spec("projects.export", "Export Project Archive", &[], r#"{"path":str,"destination":str,"wait":bool?}"#, export_cmd, true),
        spec("projects.importPackage", "Import Project Package", &[], r#"{"path":str,"folder":str?,"wait":bool?}"#, import_package_cmd, true),
        spec(
            "projects.package",
            "Package Project",
            &[],
            r#"{"path":str,"destination":str,"proxies":bool=false,"trim":bool=false,"handles":frames=30,"format":str?,"dryRun":bool=false,"wait":bool?}"#,
            package_cmd,
            true,
        ),
        spec("projects.import", "Import Project", &[], r#"{"path":str,"folder":str?,"wait":bool?}"#, import_cmd, true),
    ]
}

/// File ▸ Projects… right after File ▸ Open Project….
pub(crate) fn place(v: &mut Vec<CommandSpec>) {
    if let Some(i) = v.iter().position(|c| c.id == "projects.show") {
        let c = v.remove(i);
        let at = v.iter().position(|c| c.id == "file.open").map_or(v.len(), |j| j + 1);
        v.insert(at, c);
    }
}
