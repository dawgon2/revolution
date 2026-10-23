//! The render cache (fork Phase 6.3; like DaVinci Resolve's Smart Cache): while the user is not
//! playing, scrubbing or exporting, the background worker ([`crate::background`]) renders the
//! timeline stretches that are expensive to play into small files, and playback shows those
//! instead of rendering live.
//!
//! **What a part is.** The render bar's segments ([`filmcraft_render::preview`]) already start at
//! every clip edge, so a cut is always the start of a segment. Each segment is cached in parts of
//! [`PART_SECONDS`] (2 s) on a grid from the segment start; a part is one file named like a partial
//! render preview (`<hash>-<off>-<len>.mov`, or `<hash>.mov` for a whole short segment) in the
//! project's preview folder. A part is one closed GOP: H.265 Main 10 4:2:0 at the sequence size
//! from the in-process VA-API encoder (keyframe distance = the part, constant QP 20), composited on
//! the GPU (GPU rendering Auto). The first frame after a cut is therefore the first picture of a
//! small file: one picture to decode instead of a burst of 30. Where VA-API encoding is not
//! available (or fails) a part is ProRes 422 HQ instead.
//!
//! **Smart mode** (`mediaCache.renderCache` = `smart`, the default) caches what needs it: red and
//! yellow segments and graded clips (a segment plays natively, "no bar", only as a single ungraded,
//! unaffected clip of the sequence's size and rate, so effects and 6.2K-in-4K always count). `all`
//! caches every segment, `off` nothing. Parts are queued once the user has been idle for
//! [`IDLE_BEFORE_QUEUE`] (1 s): the parts within [`NEAR_SECONDS`] (30 s) of the playhead first
//! (nearest first), then red, then yellow, then the rest. The queue is kept short
//! ([`MAX_QUEUED`]) so it follows the playhead.
//!
//! **Invalidation** is by content hash, as for render previews: an edit changes the hash of the
//! segments it touches (their queued parts are cancelled and the new hash is queued), undo brings
//! the old hash back (its parts are green again), and relinking a clip changes the source identity
//! and so the hash.
//!
//! **Storage.** `index.json` in the preview folder lists every preview file with when playback last
//! used it and the project that made it (sizes always come from the file system: the index is
//! hostile input). Two projects with the same name can share a folder (one scratch disk), so the
//! destructive passes (size limit, Delete Unused) only ever touch files this project made; files of
//! another project, or of no known project, are left alone. Above `mediaCache.renderCacheGb`
//! (20 GB) this project's files are deleted down to [`LOW_WATER_PERCENT`] of the limit: those no
//! sequence uses first, then the least recently used, but never a part the active sequence wants
//! (that would only render it again). New parts are queued only while this project's files plus
//! the queued parts (at an estimated size) stay below that low-water mark; when the wanted parts
//! do not fit, the cache stops and `cache.status` says `limitReached`. Files found in a folder
//! without a current-version index are from an older hash version (no project of this version can
//! use them) and are deleted once when no sequence uses them. Unfinished `*.mov.part` /
//! `index.json.part` files older than [`STALE_PART`] are deleted when a folder is loaded.
//!
//! [`tick`] drives it all; the session calls it once per UI frame (`Session::poll_persistence`),
//! cheap when nothing changed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use filmcraft_export::{BitrateMode, ExportError, ExportSettings, Exporter, Format, GpuRendering, HevcEncoder, Multiplexer, Progress, Step};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::preview::{HASH_VERSION, Need, Segment};
use filmcraft_time::{FrameRate, TimeRange};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use web_time::Instant;

use crate::background::{BackgroundJob, JobId, JobKind, JobSpec, JobStep};
use crate::commands::{CommandSpec, always, str_p};
use crate::previews::{PreviewStore, gaps, parse_video_name, part_name};
use crate::{EngineError, Result, Session};

/// Length of a cache part.
pub const PART_SECONDS: f64 = 2.0;
/// Default size limit (`mediaCache.renderCacheGb`).
pub const DEFAULT_BUDGET_GB: u32 = 20;
/// The cache index in the preview folder.
pub const INDEX_FILE: &str = "index.json";
/// A larger `index.json` is not read (it is rebuilt from the folder).
pub const MAX_INDEX_BYTES: u64 = 8 << 20;
/// Parts queued on the worker at once.
pub const MAX_QUEUED: usize = 8;
/// Parts within this distance of the playhead go first.
pub const NEAR_SECONDS: f64 = 30.0;
/// How long the user must be idle before parts are queued.
pub const IDLE_BEFORE_QUEUE: Duration = Duration::from_secs(1);
/// [`tick`] does its work at most this often.
const TICK_EVERY: Duration = Duration::from_millis(250);
/// The index is written at most this often while playback touches it.
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// A part that failed this often is not queued again in this session.
const MAX_FAILURES: u32 = 2;
/// After this many H.265 failures in a row, parts are ProRes for the rest of the session.
const MAX_HEVC_FAILURES: u32 = 3;
/// Constant QP of a cached H.265 part.
pub const PART_QP: f32 = 20.0;
/// Most grid cells planned for one segment (a 10-hour segment at 60 fps has 18 000).
const MAX_CELLS: i64 = 100_000;
/// The size limit's low-water mark: over the limit, files are deleted down to this share of it,
/// and new parts are queued only below it (hysteresis: no render / delete ping-pong).
pub const LOW_WATER_PERCENT: u64 = 90;
/// An unfinished `.part` file this old is left over from a crash (a part renders in seconds).
pub const STALE_PART: Duration = Duration::from_secs(3600);

/// What the cache renders (`mediaCache.renderCache`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Segments that need it (see the module docs).
    #[default]
    Smart,
    /// Every segment.
    All,
    /// Nothing.
    Off,
}

/// The choices of `mediaCache.renderCache` (value, label).
pub const MODES: &[(&str, &str)] = &[("smart", "Smart"), ("all", "All"), ("off", "Off")];

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "smart" => Some(Self::Smart),
            "all" => Some(Self::All),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::All => "all",
            Self::Off => "off",
        }
    }
}

/// Frames of a part at `rate` ([`PART_SECONDS`], at least 1).
pub fn part_frames(rate: FrameRate) -> i64 {
    let fps = rate.as_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return 48;
    }
    ((fps * PART_SECONDS).round() as i64).clamp(1, 2400)
}

/// The grid cells `(off, len)` of a segment of `frames` frames in parts of `part` frames.
pub fn grid(frames: i64, part: i64) -> Vec<(i64, i64)> {
    let part = part.max(1);
    let mut out = Vec::new();
    let mut off = 0i64;
    while off < frames && (out.len() as i64) < MAX_CELLS {
        let len = part.min(frames.saturating_sub(off));
        out.push((off, len));
        off = off.saturating_add(part);
    }
    out
}

/// Whether the cache renders `seg` of sequence `seq` in `mode`.
pub fn wants(project: &Project, seq: ItemId, seg: &Segment, mode: Mode) -> bool {
    match mode {
        Mode::Off => false,
        Mode::All => true,
        Mode::Smart => seg.need != Need::None || graded(project, seq, seg),
    }
}

/// Whether a visible clip of `seg` has a grade (that is not bypassed).
fn graded(project: &Project, seq: ItemId, seg: &Segment) -> bool {
    let Some(q) = project.sequence(seq) else { return false };
    seg.clips.iter().any(|c| q.find_item(*c).is_some_and(|(_, it)| it.grade.as_ref().is_some_and(|g| !g.bypass && !g.nodes.is_empty())))
}

/// One cache part to render: frames `off..off + len` of a segment.
#[derive(Clone, Debug, PartialEq)]
pub struct PartTodo {
    pub hash: String,
    /// The segment's first sequence frame and length.
    pub seg_first: i64,
    pub seg_frames: i64,
    pub off: i64,
    pub len: i64,
    /// 0 near the playhead, 1 red, 2 yellow, 3 the rest.
    pub tier: u8,
    /// Frames between the playhead and the part (0 when the playhead is inside it).
    pub distance: i64,
}

impl PartTodo {
    /// The file name ([`part_name`]).
    pub fn name(&self) -> String {
        part_name(&self.hash, self.off, self.len, self.seg_frames)
    }

    /// The worker's rank (smaller first): the tier, then the distance.
    pub fn rank(&self) -> u64 {
        (u64::from(self.tier) << 48) | (self.distance.clamp(0, (1 << 48) - 1) as u64)
    }

    /// First sequence frame.
    pub fn first_frame(&self) -> i64 {
        self.seg_first.saturating_add(self.off)
    }
}

/// The parts of `seq` the cache should render in `mode`, in the order to render them: frames
/// without a preview, in grid cells, nearest the playhead (`playhead` frame) first within
/// `near` frames, then red, yellow and the rest.
pub fn plan(store: &PreviewStore, project: &Arc<Project>, seq: ItemId, playhead: i64, mode: Mode, near: i64) -> Vec<PartTodo> {
    let Some(rate) = project.sequence(seq).map(|q| q.settings.frame_rate) else { return Vec::new() };
    if mode == Mode::Off {
        return Vec::new();
    }
    let part = part_frames(rate);
    let mut out = Vec::new();
    for seg in store.segments(project, seq).iter() {
        if seg.frames <= 0 || !wants(project, seq, seg, mode) {
            continue;
        }
        let cov = store.coverage(seg);
        for (a, len) in grid(seg.frames, part) {
            for (x, y) in gaps(&cov, a, a.saturating_add(len)) {
                // file names hold u32 offsets
                if u32::try_from(x).is_err() || u32::try_from(y - x).is_err() {
                    continue;
                }
                let (first, last) = (seg.first_frame.saturating_add(x), seg.first_frame.saturating_add(y));
                let distance = if playhead < first {
                    first - playhead
                } else if playhead >= last {
                    playhead.saturating_sub(last).saturating_add(1)
                } else {
                    0
                };
                let tier = if distance <= near {
                    0
                } else {
                    match seg.need {
                        Need::Render => 1,
                        Need::Realtime => 2,
                        Need::None => 3,
                    }
                };
                out.push(PartTodo { hash: seg.hash.clone(), seg_first: seg.first_frame, seg_frames: seg.frames, off: x, len: y - x, tier, distance });
            }
        }
    }
    out.sort_by_key(|t| (t.rank(), t.first_frame()));
    out
}

// ------------------------------------------------------------------ index

/// One file of the index.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IndexEntry {
    /// Size of the file. Never read from `index.json` (hostile): taken from the file system.
    #[serde(skip_deserializing)]
    pub bytes: u64,
    /// Seconds since 1970 when playback last showed a frame of it (or when it was written).
    pub last_used: u64,
    /// The project that made it ([`owner_id`]: its file path, "" while unsaved); None = unknown
    /// (found in the folder, not listed). Only a file of the current project is ever deleted by
    /// the size limit or Delete Unused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// `index.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CacheIndex {
    /// [`HASH_VERSION`] of the files.
    pub version: String,
    pub files: BTreeMap<String, IndexEntry>,
}

/// What a folder's `index.json` turned out to be.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexRead {
    /// No index: a folder from before the index (or a new one).
    Missing,
    /// An index of another hash version: its files can't be used by this version.
    OldVersion,
    /// Too large, unreadable or not an index: someone's, but unknown whose (nothing is deleted).
    Damaged,
    Current(CacheIndex),
}

/// Read `dir/index.json` without reading more than [`MAX_INDEX_BYTES`].
pub fn read_index_full(dir: &Path) -> IndexRead {
    let f = match std::fs::File::open(dir.join(INDEX_FILE)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return IndexRead::Missing,
        Err(_) => return IndexRead::Damaged,
    };
    let Ok(md) = f.metadata() else { return IndexRead::Damaged };
    if !md.is_file() || md.len() > MAX_INDEX_BYTES {
        return IndexRead::Damaged;
    }
    let mut buf = Vec::new();
    if f.take(MAX_INDEX_BYTES.saturating_add(1)).read_to_end(&mut buf).is_err() || buf.len() as u64 > MAX_INDEX_BYTES {
        return IndexRead::Damaged;
    }
    match serde_json::from_slice::<CacheIndex>(&buf) {
        Ok(idx) if idx.version == HASH_VERSION => IndexRead::Current(idx),
        Ok(idx) if idx.version.starts_with("filmcraft-preview-") => IndexRead::OldVersion,
        _ => IndexRead::Damaged,
    }
}

/// The current-version index of `dir` (None when missing, damaged or of another version).
pub fn read_index(dir: &Path) -> Option<CacheIndex> {
    match read_index_full(dir) {
        IndexRead::Current(i) => Some(i),
        _ => None,
    }
}

/// Whether `name` is an unfinished file the cache writes (`<preview>.mov.part`, `index.json.part`).
pub fn is_partial_name(name: &str) -> bool {
    name == format!("{INDEX_FILE}.part") || name.strip_suffix(".part").is_some_and(|n| parse_video_name(n).is_some())
}

/// Delete the unfinished files ([`is_partial_name`]) in `dir` not modified for `older_than` (a
/// crash or a kill left them). Returns how many.
pub fn sweep_partials(dir: &Path, older_than: Duration) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !is_partial_name(&name) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let old = md.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= older_than);
        if md.is_file() && old && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// Who made the files the session renders now: the project file path ("" while unsaved).
pub fn owner_id(s: &Session) -> String {
    s.path.clone().unwrap_or_default()
}

/// Write `dir/index.json` (through a temporary file, so a crash never leaves half an index).
pub fn write_index(dir: &Path, files: &BTreeMap<String, IndexEntry>) -> std::io::Result<()> {
    #[derive(Serialize)]
    struct Out<'a> {
        version: &'a str,
        files: &'a BTreeMap<String, IndexEntry>,
    }
    let bytes = serde_json::to_vec(&Out { version: HASH_VERSION, files }).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{INDEX_FILE}.part"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(INDEX_FILE))
}

/// The segment hash a preview file belongs to (video part or audio preview).
pub fn owner_hash(name: &str) -> Option<&str> {
    parse_video_name(name).map(|(h, _, _)| h).or_else(|| name.strip_suffix(".wav").filter(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit())))
}

/// The order in which files are deleted: files whose hash no sequence uses first, then the least
/// recently used (ties by name, so the order is stable).
pub fn eviction_order(files: &BTreeMap<String, IndexEntry>, referenced: &HashSet<String>) -> Vec<String> {
    let mut v: Vec<(bool, u64, &String)> = files.iter().map(|(n, e)| (owner_hash(n).is_some_and(|h| referenced.contains(h)), e.last_used, n)).collect();
    v.sort();
    v.into_iter().map(|(_, _, n)| n.clone()).collect()
}

/// The files to delete to bring `files` within `budget` bytes ([`eviction_order`]), never one of a
/// `protected` segment hash (the parts the active sequence wants: deleting them would only queue
/// them again). May stay over `budget` when only protected files are left.
pub fn evict(files: &BTreeMap<String, IndexEntry>, referenced: &HashSet<String>, protected: &HashSet<String>, budget: u64) -> Vec<String> {
    let mut total: u64 = files.values().map(|e| e.bytes).fold(0, u64::saturating_add);
    let mut out = Vec::new();
    for name in eviction_order(files, referenced) {
        if total <= budget {
            break;
        }
        if owner_hash(&name).is_some_and(|h| protected.contains(h)) {
            continue;
        }
        total = total.saturating_sub(files.get(&name).map_or(0, |e| e.bytes));
        out.push(name);
    }
    out
}

/// `budget`'s low-water mark ([`LOW_WATER_PERCENT`]).
pub fn low_water(budget: u64) -> u64 {
    budget / 100 * LOW_WATER_PERCENT
}

/// The files of `index` the project `me` made (the only ones the destructive passes touch).
pub fn files_of(index: &BTreeMap<String, IndexEntry>, me: &str) -> BTreeMap<String, IndexEntry> {
    index.iter().filter(|(_, e)| e.project.as_deref() == Some(me)).map(|(n, e)| (n.clone(), e.clone())).collect()
}

/// The size a new part will probably have: the average of `index`'s video parts, else a
/// conservative guess for a `w`×`h` part of `frames` frames (4 bits a pixel).
pub fn estimated_part_bytes(index: &BTreeMap<String, IndexEntry>, w: u32, h: u32, frames: i64) -> u64 {
    let (n, sum) = index.iter().filter(|(n, _)| parse_video_name(n).is_some()).fold((0u64, 0u64), |(n, s), (_, e)| (n + 1, s.saturating_add(e.bytes)));
    if let Some(avg) = sum.checked_div(n) {
        return avg.max(1);
    }
    u64::from(w).saturating_mul(u64::from(h)).saturating_mul(frames.max(1).unsigned_abs()) / 2
}

/// The size limit in bytes for `gb` (at least 1 GB).
pub fn budget_bytes(gb: u32) -> u64 {
    u64::from(gb.max(1)).saturating_mul(1 << 30)
}

/// Hashes of every segment (video and audio) of every sequence of `project`.
pub fn referenced_hashes(project: &Project) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in project.sequences() {
        out.extend(filmcraft_render::preview::video_segments(project, item.id).into_iter().map(|g| g.hash));
        out.extend(filmcraft_render::preview::audio_segments(project, item.id).into_iter().map(|g| g.hash));
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

// ------------------------------------------------------------------ state

/// A part on the worker. The entry stays until the job itself ends (also when cancelled), so a
/// part is never submitted twice; an old job ending only removes its own entry (by job id).
#[derive(Clone, Debug)]
struct Queued {
    id: JobId,
}

/// The part being rendered.
#[derive(Clone, Debug)]
struct Running {
    job: JobId,
    hash: String,
    off: i64,
    len: i64,
    fraction: f32,
}

#[derive(Default)]
struct Inner {
    dir: Option<PathBuf>,
    index: BTreeMap<String, IndexEntry>,
    /// Files found in a folder without a (v2) index: swept once when no sequence uses them.
    legacy: HashSet<String>,
    dirty: bool,
    saved_at: Option<Instant>,
    queued: HashMap<String, Queued>,
    running: Option<Running>,
    failures: HashMap<String, u32>,
    hevc_failures: u32,
    last_tick: Option<Instant>,
    last_busy: Option<Instant>,
    plan_playhead: Option<i64>,
    swept: Option<PathBuf>,
    enforced: Option<(u64, u64)>,
    paused: bool,
    rendered: u64,
    failed: u64,
    last_error: Option<String>,
    /// The current project ([`owner_id`]); None until the first [`tick`] after a folder change.
    owner: Option<String>,
    /// The wanted parts don't fit in the size limit: nothing more is queued.
    limit_reached: bool,
    /// Tests: a size limit in bytes instead of `mediaCache.renderCacheGb`.
    budget_override: Option<u64>,
}

/// The render cache's index and queue, kept by the [`PreviewStore`] it renders into.
#[derive(Default)]
pub struct CacheState {
    inner: Mutex<Inner>,
}

impl CacheState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The store moved to `dir` holding `files`: read its index (or rebuild it from the files).
    pub(crate) fn load(&self, dir: Option<&Path>, files: &HashSet<String>) {
        let mut c = self.lock();
        if c.dir.as_deref() == dir && dir.is_some() {
            // re-indexing the same folder: keep what is known, add / drop files (new files in a
            // folder without an index are from before it)
            let mut known = std::mem::take(&mut c.index);
            match dir.map_or(IndexRead::Missing, read_index_full) {
                IndexRead::Missing | IndexRead::OldVersion => {
                    let new: Vec<String> = files.iter().filter(|n| !known.contains_key(*n)).cloned().collect();
                    c.legacy.extend(new);
                    if !c.legacy.is_empty() {
                        c.swept = None;
                    }
                }
                // what the index on disk knows (another session may have written it); what this
                // session knows wins
                IndexRead::Current(i) => {
                    for (n, e) in i.files {
                        known.entry(n).or_insert(e);
                    }
                }
                IndexRead::Damaged => {}
            }
            c.index = Self::merge(dir, files, Some(known));
            c.legacy.retain(|n| files.contains(n));
            c.dirty = true;
            return;
        }
        // a new folder: unfinished files a crash left behind go (only the cache's own patterns)
        if let Some(d) = dir {
            let n = sweep_partials(d, STALE_PART);
            if n > 0 {
                log::info!("render cache: deleted {n} unfinished file(s) in {}", d.display());
            }
        }
        let read = dir.map_or(IndexRead::Missing, read_index_full);
        // only a folder without a current index holds files of an older hash version; a damaged
        // index may be another project's: nothing in that folder is swept
        c.legacy = if matches!(read, IndexRead::Missing | IndexRead::OldVersion) { files.iter().cloned().collect() } else { HashSet::new() };
        let others = match &read {
            IndexRead::Current(i) => i.files.values().filter(|e| e.project.is_none()).count(),
            IndexRead::Damaged => files.len(),
            _ => 0,
        };
        if others > 0 {
            log::info!("render cache: {others} file(s) in this folder have no known project; they are never deleted by this project");
        }
        c.index = Self::merge(dir, files, if let IndexRead::Current(i) = read { Some(i.files) } else { None });
        c.dir = dir.map(Path::to_path_buf);
        c.dirty = true;
        c.swept = None;
        c.enforced = None;
        c.queued.clear();
        c.running = None;
        c.owner = None;
        c.limit_reached = false;
    }

    /// The index of the files `files` of `dir`: `last_used` and the project from `known` (a read
    /// index or the entries known so far), the size always from the file system.
    fn merge(dir: Option<&Path>, files: &HashSet<String>, known: Option<BTreeMap<String, IndexEntry>>) -> BTreeMap<String, IndexEntry> {
        let mut known = known.unwrap_or_default();
        let mut out = BTreeMap::new();
        for name in files {
            let path = dir.map(|d| d.join(name));
            let bytes = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map_or(0, |m| m.len());
            let entry = match known.remove(name) {
                Some(e) => IndexEntry { bytes, ..e },
                None => IndexEntry { bytes, last_used: path.as_deref().map_or(0, mtime_secs), project: None },
            };
            out.insert(name.clone(), entry);
        }
        out
    }

    /// The current project is `me` ([`owner_id`]). Files made while it was unsaved ("") become
    /// its own once it has a path (its first save took the folder along).
    pub(crate) fn set_owner(&self, me: &str) {
        let mut c = self.lock();
        if c.owner.as_deref() == Some(me) {
            return;
        }
        if !me.is_empty() {
            let mut changed = false;
            for e in c.index.values_mut().filter(|e| e.project.as_deref() == Some("")) {
                e.project = Some(me.to_string());
                changed = true;
            }
            c.dirty |= changed;
        }
        c.owner = Some(me.to_string());
    }

    /// Files of no known project become `me`'s (and are no longer swept as an older version's):
    /// how many, and their bytes.
    pub(crate) fn claim_unknown(&self, me: &str) -> (usize, u64) {
        let mut c = self.lock();
        let (mut n, mut bytes) = (0usize, 0u64);
        let mut names = Vec::new();
        for (name, e) in c.index.iter_mut().filter(|(_, e)| e.project.is_none()) {
            e.project = Some(me.to_string());
            n += 1;
            bytes = bytes.saturating_add(e.bytes);
            names.push(name.clone());
        }
        for name in &names {
            c.legacy.remove(name);
        }
        c.dirty |= n > 0;
        (n, bytes)
    }

    /// Tests: a size limit in bytes (None = the setting).
    #[cfg(test)]
    pub(crate) fn set_budget_override(&self, bytes: Option<u64>) {
        let mut c = self.lock();
        c.budget_override = bytes;
        c.enforced = None;
    }

    pub(crate) fn added(&self, name: &str, bytes: u64) {
        let mut c = self.lock();
        let project = c.owner.clone();
        c.index.insert(name.to_string(), IndexEntry { bytes, last_used: now_secs(), project });
        c.legacy.remove(name);
        c.dirty = true;
    }

    pub(crate) fn removed(&self, names: &[String]) {
        let mut c = self.lock();
        for n in names {
            c.index.remove(n);
            c.legacy.remove(n);
        }
        c.dirty = true;
    }

    /// Playback showed a frame of `name`.
    pub(crate) fn touch(&self, name: &str) {
        let now = now_secs();
        let mut c = self.lock();
        let changed = c.index.get_mut(name).is_some_and(|e| {
            let old = e.last_used;
            e.last_used = now;
            old != now
        });
        c.dirty |= changed;
    }

    /// Write the index when it changed (at most every [`SAVE_EVERY`] unless `force`).
    pub fn save(&self, force: bool) {
        let mut c = self.lock();
        let due = force || c.saved_at.is_none_or(|t| t.elapsed() >= SAVE_EVERY);
        if !c.dirty || !due {
            return;
        }
        let Some(dir) = c.dir.clone() else { return };
        if !dir.is_dir() {
            // nothing rendered here yet: no folder to write into
            return;
        }
        match write_index(&dir, &c.index) {
            Ok(()) => c.dirty = false,
            Err(e) => log::warn!("render cache index {}: {e}", dir.display()),
        }
        c.saved_at = Some(Instant::now());
    }

    /// The part being rendered: (segment hash, first frame from the segment start, frames).
    pub fn building(&self) -> Option<(String, i64, i64)> {
        self.lock().running.as_ref().map(|r| (r.hash.clone(), r.off, r.len))
    }

    /// Bytes of the files in the index.
    pub fn total_bytes(&self) -> u64 {
        self.lock().index.values().map(|e| e.bytes).fold(0, u64::saturating_add)
    }

    /// A copy of the index entries (tests, status).
    pub fn index(&self) -> BTreeMap<String, IndexEntry> {
        self.lock().index.clone()
    }

    /// Files found without an index that have not been swept yet.
    pub fn legacy(&self) -> HashSet<String> {
        self.lock().legacy.clone()
    }

    /// Names of the parts queued (or running) on the worker.
    pub fn queued(&self) -> Vec<String> {
        let mut v: Vec<String> = self.lock().queued.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn paused(&self) -> bool {
        self.lock().paused
    }

    fn set_running(&self, r: Running) {
        self.lock().running = Some(r);
    }

    fn set_fraction(&self, job: JobId, f: f32) {
        if let Some(r) = self.lock().running.as_mut().filter(|r| r.job == job) {
            r.fraction = f;
        }
    }

    /// Job `job` of part `name` ended (`error` None = done or cancelled). Only that job's own
    /// queue entry and Building state are cleared: the part may have been queued again (a new job)
    /// after the old one was cancelled.
    fn ended(&self, name: &str, job: JobId, done: bool, error: Option<String>) {
        let mut c = self.lock();
        if c.queued.get(name).is_some_and(|q| q.id == job) {
            c.queued.remove(name);
        }
        if c.running.as_ref().is_some_and(|r| r.job == job) {
            c.running = None;
        }
        if done {
            c.rendered = c.rendered.saturating_add(1);
        }
        if let Some(e) = error {
            c.failed = c.failed.saturating_add(1);
            *c.failures.entry(name.to_string()).or_default() += 1;
            log::warn!("render cache: {name}: {e}");
            c.last_error = Some(e);
        }
    }

    fn hevc_ok(&self) -> bool {
        self.lock().hevc_failures < MAX_HEVC_FAILURES
    }

    fn hevc_result(&self, ok: bool) {
        let mut c = self.lock();
        c.hevc_failures = if ok { 0 } else { c.hevc_failures.saturating_add(1) };
    }
}

// ------------------------------------------------------------------ the job

/// Export settings of a cache part of `len` frames covering `range`, written to `path`.
pub fn part_settings(format: Format, path: &str, range: TimeRange, len: i64) -> ExportSettings {
    let mut s = ExportSettings {
        format,
        path: path.to_string(),
        range: Some(range),
        scale: 1.0,
        include_audio: false,
        quality: 90,
        bitrate_kbps: 0,
        // captions stay live over previews and are not part of the hash
        burn_captions: false,
        part_of_batch: true,
        // previews stand in for the monitor picture: display-referred SDR
        sdr: true,
        // the CPU reference renderer at 6.2K is several times slower
        gpu_rendering: GpuRendering::Auto,
        ..Default::default()
    };
    if format == Format::Hevc {
        s.hevc_bit_depth = 10;
        s.hevc_encoder = HevcEncoder::Vaapi;
        s.bitrate_mode = BitrateMode::Crf;
        s.crf = PART_QP;
        // one closed GOP per file: the first picture is the only IDR
        s.keyframe_distance = Some(u32::try_from(len.max(1)).unwrap_or(u32::MAX));
        s.multiplexer = Multiplexer::Mov;
    }
    s
}

/// The format of a new part: H.265 where VA-API encoding is available and the size is even.
fn part_format(store: &PreviewStore, w: u32, h: u32) -> Format {
    let even = w.is_multiple_of(2) && h.is_multiple_of(2) && w > 0 && h > 0;
    if even && store.cache.hevc_ok() && filmcraft_export::available(Format::Hevc) { Format::Hevc } else { Format::ProRes }
}

/// The error a cancelled part's job ends with (not counted as a failure).
const CANCELLED: &str = "cancelled";

/// Whether `e` means the job was cancelled (not that the encoder failed): the worker's flag is
/// set, or the error is a cancellation (a decoder's `CodecError::Cancelled` arrives as text).
fn is_cancel(e: &ExportError) -> bool {
    filmcraft_media::cancel::cancelled() || matches!(e, ExportError::Cancelled) || e.to_string().to_ascii_lowercase().contains("cancelled")
}

/// Renders one part on the background worker.
struct CacheJob {
    store: Arc<PreviewStore>,
    project: Arc<Project>,
    seq: ItemId,
    provider: crate::media_pool::PoolProvider,
    dir: PathBuf,
    todo: PartTodo,
    name: String,
    rate: FrameRate,
    format: Format,
    exporter: Option<Exporter>,
    progress: Progress,
    ended: bool,
    /// The worker's id of this job (set by [`submit`] before the job can run; 0 = not yet).
    id: Arc<AtomicU64>,
    /// The session's Magic Mask mattes (fork G3.2; frames tracked by the time a frame renders).
    mattes: filmcraft_render::matte::MatteToken,
}

impl CacheJob {
    fn job_id(&self) -> JobId {
        self.id.load(Ordering::Relaxed)
    }

    fn partial(&self) -> PathBuf {
        self.dir.join(format!("{}.part", self.name))
    }

    fn start(&mut self) -> std::result::Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("preview folder {}: {e}", self.dir.display()))?;
        let first = self.todo.first_frame();
        let range = TimeRange::from_bounds(self.rate.tick_of(first), self.rate.tick_of(first.saturating_add(self.todo.len)));
        let path = self.partial().to_string_lossy().to_string();
        self.progress.done.store(0, Ordering::Relaxed);
        let mut first_try = part_settings(self.format, &path, range, self.todo.len);
        first_try.mattes = self.mattes;
        match Exporter::new(self.project.clone(), self.seq, &first_try, &self.progress) {
            Ok(e) => self.exporter = Some(e),
            Err(e) if is_cancel(&e) => return Err(CANCELLED.into()),
            Err(e) if self.format == Format::Hevc => {
                log::info!("render cache: H.265 part not started ({e}); ProRes instead");
                self.store.cache.hevc_result(false);
                let _ = std::fs::remove_file(self.partial());
                self.format = Format::ProRes;
                let mut s = part_settings(Format::ProRes, &path, range, self.todo.len);
                s.mattes = self.mattes;
                self.exporter = Some(Exporter::new(self.project.clone(), self.seq, &s, &self.progress).map_err(|e| e.to_string())?);
            }
            Err(e) => return Err(e.to_string()),
        }
        Ok(())
    }

    fn finish(&mut self) -> std::result::Result<(), String> {
        let fin = self.dir.join(&self.name);
        std::fs::rename(self.partial(), &fin).map_err(|e| format!("{}: {e}", fin.display()))?;
        if self.format == Format::Hevc {
            self.store.cache.hevc_result(true);
        }
        self.store.add_video(&self.name);
        Ok(())
    }

    /// The exporter stopped with `e`. A cancellation (the worker's flag, or the decoder's /
    /// exporter's Cancelled error) is not a failure: it neither counts against the part nor
    /// against H.265 (three H.265 failures switch the session to ProRes).
    fn failed(&mut self, e: ExportError) -> std::result::Result<JobStep, String> {
        self.exporter = None;
        if is_cancel(&e) {
            let _ = std::fs::remove_file(self.partial());
            self.end(false, None);
            return Err(CANCELLED.into());
        }
        if self.format == Format::Hevc {
            // VA-API encoding failed mid-part: this part again as ProRes
            log::info!("render cache: H.265 part failed ({e}); ProRes instead");
            let _ = std::fs::remove_file(self.partial());
            self.store.cache.hevc_result(false);
            self.format = Format::ProRes;
            return Ok(JobStep::More);
        }
        let e = e.to_string();
        self.end(false, Some(e.clone()));
        Err(e)
    }

    fn end(&mut self, done: bool, error: Option<String>) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.store.cache.ended(&self.name, self.job_id(), done, error);
    }
}

impl BackgroundJob for CacheJob {
    fn step(&mut self) -> std::result::Result<JobStep, String> {
        if self.exporter.is_none() {
            self.store.cache.set_running(Running { job: self.job_id(), hash: self.todo.hash.clone(), off: self.todo.off, len: self.todo.len, fraction: 0.0 });
            if let Err(e) = self.start() {
                let failure = (e != CANCELLED && !filmcraft_media::cancel::cancelled()).then(|| e.clone());
                self.end(false, failure);
                return Err(e);
            }
            return Ok(JobStep::More);
        }
        let Some(ex) = self.exporter.as_mut() else { return Err("internal: no exporter".into()) };
        match ex.step(&self.provider, &self.progress) {
            Ok(Step::Progress | Step::Pending) => {
                self.store.cache.set_fraction(self.job_id(), self.fraction());
                Ok(JobStep::More)
            }
            Ok(Step::Done(_)) => {
                self.exporter = None;
                match self.finish() {
                    Ok(()) => {
                        self.end(true, None);
                        Ok(JobStep::Done)
                    }
                    Err(e) => {
                        self.end(false, Some(e.clone()));
                        Err(e)
                    }
                }
            }
            Err(e) => self.failed(e),
        }
    }

    fn fraction(&self) -> f32 {
        let len = self.todo.len.max(1) as f32;
        (self.progress.done.load(Ordering::Relaxed) as f32 / len).clamp(0.0, 1.0)
    }

    fn cancelled(&mut self) {
        self.exporter = None;
        let _ = std::fs::remove_file(self.partial());
        self.end(false, None);
    }
}

impl Drop for CacheJob {
    fn drop(&mut self) {
        // a job dropped without finishing (the worker stopped): no partial file, not queued
        if !self.ended {
            self.exporter = None;
            let _ = std::fs::remove_file(self.partial());
            self.end(false, None);
        }
    }
}

/// Queue `todo` of sequence `seq` on the worker.
fn submit(s: &Session, seq: ItemId, todo: PartTodo, dir: &Path) -> std::result::Result<JobId, String> {
    let q = s.project.sequence(seq).ok_or("no sequence")?;
    let store = s.previews.clone();
    let format = part_format(&store, q.settings.width, q.settings.height);
    let name = todo.name();
    let job_id = Arc::new(AtomicU64::new(0));
    let mut spec = JobSpec::new(JobKind::Cache, format!("Render cache {name}"));
    spec.rank = todo.rank();
    let job = CacheJob {
        store: store.clone(),
        project: s.project.clone(),
        seq,
        // the cache is reused with proxies on or off: always full resolution
        provider: s.media.full_res_provider(s.project.clone(), s.services.clone()),
        dir: dir.to_path_buf(),
        name: name.clone(),
        todo,
        rate: q.settings.frame_rate,
        format,
        exporter: None,
        progress: Progress::default(),
        mattes: s.magic_mask.token(),
        ended: false,
        id: job_id.clone(),
    };
    // registered (and the job's id set) before the job can end: ending takes this lock
    let mut c = store.cache.lock();
    let id = s.background.submit(spec, Box::new(job))?;
    job_id.store(id, Ordering::Relaxed);
    c.queued.insert(name, Queued { id });
    Ok(id)
}

// ------------------------------------------------------------------ tick

fn mode_of(s: &Session) -> Mode {
    Mode::parse(&s.prefs.media_cache.render_cache).unwrap_or_default()
}

/// Cancel the queued parts `names` (all when None). Their entries stay until each job ends
/// (every [`CacheJob`] ends: done, failed, cancelled or dropped), so a part being stopped is not
/// queued a second time meanwhile.
fn cancel(s: &Session, names: Option<&[String]>) -> usize {
    let ids: Vec<JobId> = {
        let c = s.previews.cache.lock();
        c.queued.iter().filter(|(n, _)| names.is_none_or(|ns| ns.contains(n))).map(|(_, q)| q.id).collect()
    };
    ids.into_iter().filter(|id| s.background.cancel(*id)).count()
}

/// Drive the render cache (see the module docs): sweep, follow edits and the playhead, queue
/// parts while the user is idle, keep the size limit, save the index. `force` skips the
/// [`TICK_EVERY`] throttle and the idle wait (commands, tests, the bench).
pub fn tick(s: &mut Session, force: bool) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    let store = s.previews.clone();
    let now = Instant::now();
    {
        let mut c = store.cache.lock();
        if !force && c.last_tick.is_some_and(|t| now.saturating_duration_since(t) < TICK_EVERY) {
            return;
        }
        c.last_tick = Some(now);
    }
    let Some(dir) = store.dir() else { return };
    let mode = mode_of(s);
    let budget = store.cache.lock().budget_override.unwrap_or_else(|| budget_bytes(s.prefs.media_cache.render_cache_gb));
    let me = owner_id(s);
    store.cache.set_owner(&me);

    // previews of an old hash version that no sequence uses (once per folder; only in a folder
    // without a current index, so no project of this version can use them)
    let sweep = {
        let c = store.cache.lock();
        c.swept.as_deref() != Some(dir.as_path()) && s.project.sequences().next().is_some()
    };
    if sweep {
        let legacy = store.cache.legacy();
        if !legacy.is_empty() {
            let referenced = referenced_hashes(&s.project);
            let victims: Vec<String> = legacy.into_iter().filter(|n| owner_hash(n).is_none_or(|h| !referenced.contains(h))).collect();
            let n = store.delete_files(&victims);
            if n > 0 {
                log::info!("render cache: deleted {n} preview file(s) of an older version no sequence uses");
            }
        }
        let mut c = store.cache.lock();
        c.legacy.clear();
        c.swept = Some(dir.clone());
        c.dirty = true;
    }

    // what the active sequence shows is never deleted to make room (it would be rendered again)
    let active = s.state.active_sequence.filter(|q| s.project.sequence(*q).is_some());
    let protected: HashSet<String> = active.map(|q| store.segments(&s.project, q).iter().map(|g| g.hash.clone()).collect()).unwrap_or_default();

    // the size limit (when files were added or the limit changed): this project's files only,
    // down to the low-water mark
    let gen_now = store.generation.load(Ordering::Relaxed);
    let enforce = store.cache.lock().enforced != Some((gen_now, budget));
    if enforce {
        let mine = files_of(&store.cache.index(), &me);
        if mine.values().map(|e| e.bytes).fold(0, u64::saturating_add) > budget {
            let referenced = referenced_hashes(&s.project);
            let victims = evict(&mine, &referenced, &protected, low_water(budget));
            let n = store.delete_files(&victims);
            log::info!("render cache: over the size limit, deleted {n} file(s)");
        }
        store.cache.lock().enforced = Some((store.generation.load(Ordering::Relaxed), budget));
    }

    let paused = store.cache.paused();
    if mode == Mode::Off || paused {
        cancel(s, None);
        store.cache.lock().limit_reached = false;
        store.cache.save(false);
        return;
    }
    let Some(seq) = active else {
        cancel(s, None);
        store.cache.save(false);
        return;
    };
    let rate = s.sequence_rate();
    let playhead = rate.frame_at(s.playhead());
    let near = (rate.as_f64() * NEAR_SECONDS).round().max(0.0) as i64;
    let todo = plan(&store, &s.project, seq, playhead, mode, near);

    // edits: parts no longer wanted (their hash changed, or rendered meanwhile) are cancelled
    let wanted: HashSet<String> = todo.iter().map(PartTodo::name).collect();
    let stale: Vec<String> = store.cache.queued().into_iter().filter(|n| !wanted.contains(n)).collect();
    if !stale.is_empty() {
        cancel(s, Some(&stale));
    }

    let status = s.background.status();
    let busy = status.activity.playing || status.activity.scrubbing || status.activity.export_running || crate::background::foreground_jobs_running(s);
    let idle = {
        let mut c = store.cache.lock();
        if busy {
            c.last_busy = Some(now);
        }
        force || c.last_busy.is_none_or(|t| now.saturating_duration_since(t) >= IDLE_BEFORE_QUEUE)
    };

    // the playhead moved far: requeue in the new order (parts not started yet)
    let part = part_frames(rate);
    let moved = store.cache.lock().plan_playhead.is_none_or(|p| p.abs_diff(playhead) > part.unsigned_abs().saturating_mul(2));
    if moved && idle {
        let unstarted: HashSet<JobId> = status.jobs.iter().filter(|j| j.spec.kind == JobKind::Cache && !j.started).map(|j| j.id).collect();
        let names: Vec<String> = store.cache.lock().queued.iter().filter(|(_, q)| unstarted.contains(&q.id)).map(|(n, _)| n.clone()).collect();
        if !names.is_empty() {
            cancel(s, Some(&names));
        }
        store.cache.lock().plan_playhead = Some(playhead);
    }

    if idle {
        queue_within_limit(s, seq, todo, &dir, &me, budget, &protected);
    }
    store.cache.save(false);
}

/// Queue the parts of `todo` that are not queued yet, while this project's files plus the queued
/// parts (at an estimated size) stay below the low-water mark of `budget`. Files of this project
/// that are not `protected` are deleted to make room; when the parts still don't fit, nothing is
/// queued and `limit_reached` is set (no render / delete loop).
fn queue_within_limit(s: &Session, seq: ItemId, todo: Vec<PartTodo>, dir: &Path, me: &str, budget: u64, protected: &HashSet<String>) {
    let store = &s.previews;
    let (queued, failures): (HashSet<String>, HashMap<String, u32>) = {
        let c = store.cache.lock();
        (c.queued.keys().cloned().collect(), c.failures.clone())
    };
    let candidates: Vec<PartTodo> =
        todo.into_iter().filter(|t| !queued.contains(&t.name()) && failures.get(&t.name()).is_none_or(|f| *f < MAX_FAILURES)).collect();
    let free_slots = MAX_QUEUED.saturating_sub(queued.len());
    if candidates.is_empty() || free_slots == 0 {
        if candidates.is_empty() {
            store.cache.lock().limit_reached = false;
        }
        return;
    }
    let (w, h) = s.project.sequence(seq).map_or((0, 0), |q| (q.settings.width, q.settings.height));
    let low = low_water(budget);
    let mine_of = || {
        let mine = files_of(&store.cache.index(), me);
        let total = mine.values().map(|e| e.bytes).fold(0, u64::saturating_add);
        (mine, total)
    };
    let (mine, mut total) = mine_of();
    let est = estimated_part_bytes(&mine, w, h, part_frames(s.sequence_rate()));
    let in_queue = est.saturating_mul(queued.len() as u64);
    let need = est.saturating_mul(candidates.len().min(free_slots) as u64);
    if total.saturating_add(in_queue).saturating_add(need) > low {
        // make room: files the active sequence doesn't show (unused first, then least recent)
        let referenced = referenced_hashes(&s.project);
        let victims = evict(&mine, &referenced, protected, low.saturating_sub(in_queue).saturating_sub(need));
        if !victims.is_empty() {
            let n = store.delete_files(&victims);
            log::info!("render cache: deleted {n} file(s) to make room for the active sequence");
            total = mine_of().1;
        }
    }
    let mut inflight = total.saturating_add(in_queue);
    let mut submitted = 0usize;
    let mut blocked = false;
    for t in candidates {
        if submitted >= free_slots {
            break;
        }
        if inflight.saturating_add(est) > low {
            blocked = true;
            break;
        }
        match submit(s, seq, t, dir) {
            Ok(_) => {
                submitted += 1;
                inflight = inflight.saturating_add(est);
            }
            Err(e) => {
                log::warn!("render cache: {e}");
                break;
            }
        }
    }
    let mut c = store.cache.lock();
    if blocked && submitted == 0 && !c.limit_reached {
        log::info!("render cache: the size limit is reached; nothing more is rendered for this sequence");
    }
    c.limit_reached = blocked && submitted == 0;
}

// ------------------------------------------------------------------ commands

/// `cache.status`.
pub fn status_json(s: &Session) -> Value {
    let store = &s.previews;
    let (queued, running, rendered, failed, last_error, paused, limit_reached, budget) = {
        let c = store.cache.lock();
        let budget = c.budget_override.unwrap_or_else(|| budget_bytes(s.prefs.media_cache.render_cache_gb));
        (c.queued.len(), c.running.clone(), c.rendered, c.failed, c.last_error.clone(), c.paused, c.limit_reached, budget)
    };
    let index = store.cache.index();
    let parts = index.keys().filter(|n| parse_video_name(n).is_some()).count();
    let bytes: u64 = index.values().map(|e| e.bytes).fold(0, u64::saturating_add);
    let me = owner_id(s);
    let other_files = index.values().filter(|e| e.project.as_deref() != Some(me.as_str())).count();
    let unknown_files = index.values().filter(|e| e.project.is_none()).count();
    let worker = s.background.status();
    json!({
        "mode": mode_of(s).as_str(),
        "paused": paused,
        "parts": parts,
        "bytes": bytes,
        "budgetBytes": budget,
        "limitReached": limit_reached,
        "otherFiles": other_files,
        "unknownFiles": unknown_files,
        "queued": queued,
        "rendering": running.map(|r| json!({"hash": r.hash, "off": r.off, "frames": r.len, "percent": (r.fraction * 1000.0).round() / 10.0})),
        "rendered": rendered,
        "failed": failed,
        "lastError": last_error,
        "worker": worker.state,
        "dir": store.dir().map(|d| d.to_string_lossy().to_string()),
    })
}

fn set_mode(s: &mut Session, p: &Value) -> Result<Value> {
    let m = str_p(p, "mode")
        .and_then(Mode::parse)
        .ok_or_else(|| EngineError::BadParams { cmd: "cache.setMode".into(), msg: "`mode` must be smart, all or off".into() })?;
    let mut next = s.prefs.clone();
    next.media_cache.render_cache = m.as_str().into();
    s.set_prefs(next).map_err(|e| EngineError::Other(e.to_string()))?;
    tick(s, true);
    Ok(status_json(s))
}

/// Delete Unused: this project's files whose hash no sequence of the project uses (undo may bring
/// such a hash back, so the size limit keeps them until it needs the room). Files of another
/// project sharing the folder, or of no known project, are never deleted.
fn clear_unused(s: &mut Session) -> Value {
    let referenced = referenced_hashes(&s.project);
    let me = owner_id(s);
    s.previews.cache.set_owner(&me);
    let index = files_of(&s.previews.cache.index(), &me);
    let victims: Vec<String> = index.keys().filter(|n| owner_hash(n).is_some_and(|h| !referenced.contains(h))).cloned().collect();
    let bytes: u64 = victims.iter().filter_map(|n| index.get(n)).map(|e| e.bytes).fold(0, u64::saturating_add);
    let n = s.previews.delete_files(&victims);
    s.previews.cache.save(true);
    json!({"deleted": n, "bytes": bytes})
}

/// Claim the folder: files of no known project (from before the index, or listed without a project)
/// become the current project's, so Delete Unused and the size limit may delete them once no
/// sequence uses them. Files another project made stay that project's.
fn claim_folder(s: &mut Session) -> Result<Value> {
    if s.previews.dir().is_none() {
        return Err(EngineError::BadParams { cmd: "cache.claimFolder".into(), msg: "no render cache folder".into() });
    }
    let me = owner_id(s);
    s.previews.cache.set_owner(&me);
    let (claimed, bytes) = s.previews.cache.claim_unknown(&me);
    s.previews.cache.save(true);
    Ok(json!({"claimed": claimed, "bytes": bytes}))
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>, journal: bool) -> CommandSpec {
        CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal }
    }
    vec![
        spec("cache.status", "Render Cache Status", "{}", |s, _| Ok(status_json(s)), false),
        spec("cache.setMode", "Set Render Cache Mode", r#"{"mode":"smart|all|off"}"#, set_mode, true),
        spec(
            "cache.pause",
            "Pause Render Cache",
            "{}",
            |s, _| {
                s.previews.cache.lock().paused = true;
                cancel(s, None);
                Ok(status_json(s))
            },
            false,
        ),
        spec(
            "cache.resume",
            "Resume Render Cache",
            "{}",
            |s, _| {
                s.previews.cache.lock().paused = false;
                tick(s, true);
                Ok(status_json(s))
            },
            false,
        ),
        spec("cache.clearUnused", "Delete Unused Render Cache Files", "{}", |s, _| Ok(clear_unused(s)), true),
        spec("cache.claimFolder", "Claim Render Cache Files of No Known Project", "{}", |s, _| claim_folder(s), true),
    ]
}

#[cfg(test)]
#[path = "render_cache_tests.rs"]
mod tests;
