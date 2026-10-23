//! Render previews (Sequence ▸ Render Effects In to Out, Render In to Out, Render Selection,
//! Delete Render Files…) and the timeline render bar.
//!
//! Segments, content hashes and the yellow/red cost estimate come from
//! [`filmcraft_render::preview`]. A rendered segment is a ProRes 422 QuickTime file named
//! `<hash>.mov` in the project's preview cache directory, and a rendered part of a segment (In/Out
//! inside it: Render In to Out renders only In to Out, like Premiere) is `<hash>-<off>-<len>.mov`,
//! frames `off..off + len` counted from the segment start. Because the offsets are relative to the
//! segment, a ripple edit that moves the segment keeps its parts, as it keeps whole previews. The
//! render bar is green over the rendered frames only; later renders fill the gaps.
//!
//! The folder:
//!
//! * saved project `/path/Film.fcproj` → `/path/FilmCraft Previews/Film/`;
//! * unsaved project → a per-process folder in the system temp dir; its files move into the
//!   project's folder on the first save.
//!
//! Because files are named by content, an edit simply changes the hash of the segments it touches
//! (their bar turns yellow/red again) while every other preview stays valid; undo brings the old
//! hash back and the segment is green again. Files are written as `<hash>.mov.part` and renamed
//! when complete, so an interrupted render never leaves a bad preview behind.
//!
//! Playback asks [`PreviewStore::frame`] first: when the frame lies in a segment with a preview, the
//! decoded preview frame replaces the live render.
//!
//! The render cache (fork 6.3, [`crate::render_cache`]) writes its parts into the same folder with
//! the same names, so the bar, playback and Delete Render Files treat them like any preview. The
//! folder's `index.json` records each file's size and when playback last used it (for the cache's
//! size limit).

use filmcraft_audio_dsp::channels::{Layout, Mixdown};
use filmcraft_render::audio::to_layout;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, SharedSource};
use filmcraft_project::{ClipId, ItemId, Project};
use filmcraft_render::preview::{AudioSegment, Need, Segment, segment_at, video_segments};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{EngineError, MediaPool, Result, Session};

/// Render-bar colour of one segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BarState {
    /// Plays natively, nothing drawn.
    None,
    Yellow,
    Red,
    /// A preview file exists.
    Green,
    /// The render cache is rendering these frames right now (fork 6.3).
    Building,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BarSpan {
    pub start: Tick,
    pub end: Tick,
    pub state: BarState,
}

/// Which segments a render command picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    /// Red and yellow segments in In/Out (Enter).
    EffectsInToOut,
    /// Every segment in In/Out, including no-bar ones.
    InToOut,
    /// Segments showing a selected clip.
    Selection,
}

/// (project snapshot, sequence, matte-cache epoch, segments)
type Memo = (Arc<Project>, ItemId, u64, Arc<Vec<Segment>>);
type AudioMemo = (Arc<Project>, ItemId, Arc<Vec<AudioSegment>>);

/// Preview files of one project, plus memoized segments for the latest project snapshots.
#[derive(Default)]
pub struct PreviewStore {
    dir: RwLock<Option<PathBuf>>,
    /// Where unsaved projects' previews go (Settings ▸ Media Cache); None = the system temp dir.
    temp_root: RwLock<Option<PathBuf>>,
    /// File names present: `<hash>.mov` / `<hash>-<off>-<len>.mov` (video) and `<hash>.wav` (audio).
    files: RwLock<HashSet<String>>,
    /// The video files of `files` by segment hash (rebuilt whenever `files` changes).
    video: RwLock<HashMap<String, Vec<Part>>>,
    /// Opened preview files by file name (most recent last).
    sources: Mutex<Vec<(String, SharedSource)>>,
    /// Loaded audio previews: interleaved stereo f32 (most recent last).
    audio: Mutex<Vec<(String, Arc<Vec<f32>>)>>,
    memo: Mutex<Vec<Memo>>,
    audio_memo: Mutex<Vec<AudioMemo>>,
    /// Bumped whenever the set of preview files changes.
    pub generation: AtomicU64,
    /// Live mixer state shared with playback: held controls, meters, newest project snapshot.
    pub live: Arc<filmcraft_render::mixer::LiveMix>,
    /// The render cache's index and queue (fork 6.3).
    pub cache: crate::render_cache::CacheState,
    /// The session's Magic Mask mattes (fork G3.2): a segment showing a node with a Magic Mask
    /// has its matte epochs in its hash, so a part rendered before (or while) the node was
    /// tracked is not reused after ([`Self::set_mattes`]).
    mattes: RwLock<Option<Arc<crate::magic_mask::MatteCache>>>,
}

/// Opened preview files kept at once (playback pre-opens the next part, so the part on screen,
/// the next one and a few recent ones stay open).
const MAX_OPEN_PARTS: usize = 12;

/// A preview file no streaming reader takes is read whole only up to this size (fork 6.3: the
/// store used to read every file whole).
const MAX_WHOLE_READ: u64 = 64 << 20;

/// Playback opens the next part when the frame on screen is this close to the end of its part.
const PREOPEN_FRAMES: i64 = 30;

fn video_name(hash: &str) -> String {
    format!("{hash}.mov")
}

/// A rendered stretch of one segment: frames `off..off + len` counted from the segment start, in
/// file `name`. A whole-segment preview has `off` 0 and `len` [`WHOLE`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Part {
    off: i64,
    len: i64,
    name: String,
}

const WHOLE: i64 = i64::MAX;

/// File name of the preview of frames `off..off + len` of a segment of `frames` frames.
pub(crate) fn part_name(hash: &str, off: i64, len: i64, frames: i64) -> String {
    if off == 0 && len >= frames { video_name(hash) } else { format!("{hash}-{off}-{len}.mov") }
}

/// `(hash, off, len)` of a video preview file name (`<hash>.mov` or `<hash>-<off>-<len>.mov`).
pub(crate) fn parse_video_name(name: &str) -> Option<(&str, i64, i64)> {
    let stem = name.strip_suffix(".mov")?;
    if is_hash(stem) {
        return Some((stem, 0, WHOLE));
    }
    let mut it = stem.split('-');
    let (hash, off, len) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || !is_hash(hash) {
        return None;
    }
    let (off, len) = (off.parse::<u32>().ok()?, len.parse::<u32>().ok()?);
    (len > 0).then_some((hash, i64::from(off), i64::from(len)))
}

/// The parts of `[a, b)` not covered by the sorted, merged intervals `cov`.
pub(crate) fn gaps(cov: &[(i64, i64)], a: i64, b: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut pos = a;
    for &(x, y) in cov {
        if y <= pos {
            continue;
        }
        if x >= b {
            break;
        }
        if x > pos {
            out.push((pos, x));
        }
        pos = pos.max(y);
    }
    if pos < b {
        out.push((pos, b));
    }
    out
}

/// First frame whose start is at or after `t`.
fn frame_ceil(rate: FrameRate, t: Tick) -> i64 {
    let f = rate.frame_at(t);
    if rate.tick_of(f) < t { f.saturating_add(1) } else { f }
}

/// Frames of In/Out (`range`) inside `seg`, counted from the segment start (empty when apart).
fn rel_range(seg: &Segment, rate: FrameRate, range: &TimeRange) -> (i64, i64) {
    let a = rate.frame_at(range.start).saturating_sub(seg.first_frame).clamp(0, seg.frames.max(0));
    let b = frame_ceil(rate, range.end()).saturating_sub(seg.first_frame).clamp(0, seg.frames.max(0));
    (a, b.max(a))
}
fn audio_name(hash: &str) -> String {
    format!("{hash}.wav")
}

impl PreviewStore {
    /// A store using a fresh per-process temp folder (unsaved projects).
    pub fn temp() -> Self {
        let s = Self::default();
        s.set_dir(default_temp_dir());
        s
    }

    /// Switch to a fresh temp folder (new unsaved project).
    pub fn reset_temp(&self) {
        let root = self.temp_root();
        self.set_dir(match root {
            Some(r) => Some(untitled_dir(&r)),
            None => default_temp_dir(),
        });
    }

    /// The folder unsaved projects' previews go into (None = the system temp dir).
    pub fn temp_root(&self) -> Option<PathBuf> {
        self.temp_root.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_temp_root(&self, root: Option<PathBuf>) {
        *self.temp_root.write().unwrap_or_else(|e| e.into_inner()) = root;
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.dir.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Point the store at `dir` and index the previews already there.
    pub fn set_dir(&self, dir: Option<PathBuf>) {
        let mut files = HashSet::new();
        if let Some(d) = &dir
            && let Ok(rd) = std::fs::read_dir(d)
        {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if parse_video_name(&name).is_some() || name.strip_suffix(".wav").is_some_and(is_hash) {
                    files.insert(name);
                }
            }
        }
        self.cache.load(dir.as_deref(), &files);
        *self.dir.write().unwrap_or_else(|e| e.into_inner()) = dir;
        *self.files.write().unwrap_or_else(|e| e.into_inner()) = files;
        self.reindex();
        self.sources.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.audio.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.bump();
    }

    /// Move to `dir`, taking this store's preview files along (first save of a project).
    pub fn move_to(&self, dir: PathBuf) {
        let old = self.dir();
        if old.as_deref() == Some(dir.as_path()) {
            return;
        }
        if let Some(old) = &old
            && (old.starts_with(crate::temp_dir()) || self.temp_root().is_some_and(|r| old.starts_with(r)))
        {
            let _ = std::fs::create_dir_all(&dir);
            // the cache index goes along (written first, so it lists every file that moves)
            self.cache.save(true);
            let index = [crate::render_cache::INDEX_FILE.to_string()];
            for name in self.files.read().unwrap_or_else(|e| e.into_inner()).iter().chain(index.iter()) {
                let (a, b) = (old.join(name), dir.join(name));
                if !b.exists() && std::fs::rename(&a, &b).is_err() {
                    let _ = std::fs::copy(&a, &b).map(|_| std::fs::remove_file(&a));
                }
            }
        }
        self.set_dir(Some(dir));
    }

    /// Rebuild the by-hash index of the video previews from the file names.
    fn reindex(&self) {
        let mut video: HashMap<String, Vec<Part>> = HashMap::new();
        for name in self.files.read().unwrap_or_else(|e| e.into_inner()).iter() {
            if let Some((hash, off, len)) = parse_video_name(name) {
                video.entry(hash.to_string()).or_default().push(Part { off, len, name: name.clone() });
            }
        }
        *self.video.write().unwrap_or_else(|e| e.into_inner()) = video;
    }

    /// The rendered frames of `seg`, counted from its start: sorted, merged, within the segment.
    pub fn coverage(&self, seg: &Segment) -> Vec<(i64, i64)> {
        let video = self.video.read().unwrap_or_else(|e| e.into_inner());
        let Some(parts) = video.get(&seg.hash) else { return Vec::new() };
        let n = seg.frames.max(0);
        let mut iv: Vec<(i64, i64)> = parts.iter().map(|p| (p.off.clamp(0, n), p.off.saturating_add(p.len).clamp(0, n))).filter(|(a, b)| a < b).collect();
        iv.sort_unstable();
        let mut out: Vec<(i64, i64)> = Vec::new();
        for (a, b) in iv {
            match out.last_mut() {
                Some(l) if a <= l.1 => l.1 = l.1.max(b),
                _ => out.push((a, b)),
            }
        }
        out
    }

    /// Whether previews cover every frame of `seg`.
    pub fn is_rendered(&self, seg: &Segment) -> bool {
        matches!(self.coverage(seg).as_slice(), [(0, b)] if *b == seg.frames)
    }

    /// Names of the video preview files of `seg` that hold frames of `a..b` (from the segment start).
    fn parts_in(&self, seg: &Segment, a: i64, b: i64) -> Vec<String> {
        if a >= b {
            return Vec::new();
        }
        let video = self.video.read().unwrap_or_else(|e| e.into_inner());
        let Some(parts) = video.get(&seg.hash) else { return Vec::new() };
        parts.iter().filter(|p| p.off < b && a < p.off.saturating_add(p.len)).map(|p| p.name.clone()).collect()
    }

    /// Whether the audio preview of audio segment `hash` exists.
    pub fn has_audio(&self, hash: &str) -> bool {
        self.files.read().unwrap_or_else(|e| e.into_inner()).contains(&audio_name(hash))
    }

    /// Number of preview files (video and audio).
    pub fn count(&self) -> usize {
        self.files.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn audio_path_for(&self, hash: &str) -> Option<PathBuf> {
        self.dir().map(|d| d.join(audio_name(hash)))
    }

    /// Register a finished video preview file (`<hash>.mov` or `<hash>-<off>-<len>.mov`).
    pub(crate) fn add_video(&self, name: &str) {
        self.files.write().unwrap_or_else(|e| e.into_inner()).insert(name.to_string());
        self.reindex();
        let bytes = self.dir().and_then(|d| std::fs::metadata(d.join(name)).ok()).map_or(0, |m| m.len());
        self.cache.added(name, bytes);
        self.bump();
    }

    /// Register a finished audio preview file.
    pub fn add_audio(&self, hash: &str) {
        let name = audio_name(hash);
        self.files.write().unwrap_or_else(|e| e.into_inner()).insert(name.clone());
        let bytes = self.dir().and_then(|d| std::fs::metadata(d.join(&name)).ok()).map_or(0, |m| m.len());
        self.cache.added(&name, bytes);
        self.bump();
    }

    /// Delete the preview files (video and audio) of `hashes`, or every file when None. Returns
    /// how many files were removed.
    pub fn delete(&self, hashes: Option<&[String]>) -> usize {
        let victims: Vec<String> = {
            let files = self.files.read().unwrap_or_else(|e| e.into_inner());
            match hashes {
                Some(h) => files
                    .iter()
                    .filter(|n| {
                        let owner = parse_video_name(n).map(|(hash, _, _)| hash).or_else(|| n.strip_suffix(".wav"));
                        owner.is_some_and(|o| h.iter().any(|h| h == o))
                    })
                    .cloned()
                    .collect(),
                None => files.iter().cloned().collect(),
            }
        };
        self.delete_files(&victims)
    }

    /// Delete the preview files named `names`. Returns how many were removed.
    pub(crate) fn delete_files(&self, names: &[String]) -> usize {
        let Some(dir) = self.dir() else { return 0 };
        let victims: Vec<String> = {
            let files = self.files.read().unwrap_or_else(|e| e.into_inner());
            names.iter().filter(|n| files.contains(*n)).cloned().collect()
        };
        self.sources.lock().unwrap_or_else(|e| e.into_inner()).retain(|(n, _)| !victims.contains(n));
        self.audio.lock().unwrap_or_else(|e| e.into_inner()).retain(|(h, _)| !victims.contains(&audio_name(h)));
        let mut files = self.files.write().unwrap_or_else(|e| e.into_inner());
        for n in &victims {
            let _ = std::fs::remove_file(dir.join(n));
            files.remove(n);
        }
        drop(files);
        self.reindex();
        self.cache.removed(&victims);
        self.bump();
        victims.len()
    }

    /// Audio segments of `seq` in this project snapshot (memoized per snapshot).
    pub fn audio_segments(&self, project: &Arc<Project>, seq: ItemId) -> Arc<Vec<AudioSegment>> {
        let mut memo = self.audio_memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, s)) = memo.iter().find(|(p, q, _)| Arc::ptr_eq(p, project) && *q == seq) {
            return s.clone();
        }
        let segs = Arc::new(filmcraft_render::preview::audio_segments(project, seq));
        memo.push((project.clone(), seq, segs.clone()));
        if memo.len() > 4 {
            memo.remove(0);
        }
        segs
    }

    fn load_audio(&self, hash: &str) -> Option<Arc<Vec<f32>>> {
        {
            let mut g = self.audio.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = g.iter().position(|(h, _)| h == hash) {
                let e = g.remove(i);
                let s = e.1.clone();
                g.push(e);
                return Some(s);
            }
        }
        let bytes = std::fs::read(self.audio_path_for(hash)?).ok()?;
        let samples = Arc::new(read_wav_f32(&bytes)?);
        let mut g = self.audio.lock().unwrap_or_else(|e| e.into_inner());
        g.push((hash.to_string(), samples.clone()));
        if g.len() > 32 {
            g.remove(0);
        }
        Some(samples)
    }

    /// Sequence audio for playback and meters, as stereo (a 5.1 Mix folded with the BS.775
    /// downmix): rendered audio previews where they are valid, the live mix everywhere else.
    pub fn mix(&self, project: &Arc<Project>, seq: ItemId, start: i64, frames: usize, sources: &dyn filmcraft_render::SourceProvider) -> AudioBuffer {
        self.mix_layout(project, seq, start, frames, sources, Layout::Stereo, Mixdown::FrontRear)
    }

    /// [`PreviewStore::mix`] in `layout`: a 5.1 Mix is folded to stereo with `mixdown` (Preferences ▸
    /// Audio ▸ 5.1 Mixdown Type) or played as six channels on a 5.1 device; a stereo Mix is placed
    /// on the front speakers of a 5.1 device. Rendered audio previews are stereo BS.775 folds, so
    /// they are used only for stereo output of a stereo Mix or with the BS.775 mixdown.
    #[allow(clippy::too_many_arguments)]
    pub fn mix_layout(
        &self,
        project: &Arc<Project>,
        seq: ItemId,
        start: i64,
        frames: usize,
        sources: &dyn filmcraft_render::SourceProvider,
        layout: Layout,
        mixdown: Mixdown,
    ) -> AudioBuffer {
        let Some(q) = project.sequence(seq) else { return AudioBuffer::silence(48_000, layout.channels(), frames) };
        let live = Some(&*self.live);
        let has_audio = self.files.read().unwrap_or_else(|e| e.into_inner()).iter().any(|n| n.ends_with(".wav"));
        let surround = q.settings.audio_master == filmcraft_project::AudioChannels::Surround51;
        let previews_ok = layout == Layout::Stereo && (!surround || mixdown == Mixdown::FrontRear);
        // held mixer controls are heard live (rendered previews don't know them)
        if !has_audio || self.live.is_active() || !previews_ok {
            return to_layout(filmcraft_render::mixer::mix_graph(project, q, start, frames, sources, live), layout, mixdown);
        }
        let segs = self.audio_segments(project, seq);
        let mut out = AudioBuffer::silence(q.settings.sample_rate, 2, frames);
        let end = start + frames as i64;
        let mut pos = start;
        while pos < end {
            let i = segs.partition_point(|g| g.first_sample <= pos);
            let seg = i.checked_sub(1).map(|i| &segs[i]).filter(|g| pos < g.first_sample + g.samples);
            let preview = seg.filter(|g| self.has_audio(&g.hash)).and_then(|g| self.load_audio(&g.hash).map(|a| (g, a)));
            let next = match seg {
                Some(g) => (g.first_sample + g.samples).min(end),
                None => segs.get(i).map(|g| g.first_sample).unwrap_or(end).min(end),
            };
            let n = (next - pos).max(1) as usize;
            let off = (pos - start) as usize;
            match preview {
                Some((g, a)) if a.len() >= (g.samples as usize) * 2 => {
                    let k0 = (pos - g.first_sample) as usize;
                    for k in 0..n {
                        out.channels[0][off + k] = a[(k0 + k) * 2];
                        out.channels[1][off + k] = a[(k0 + k) * 2 + 1];
                    }
                }
                _ => {
                    let b = to_layout(filmcraft_render::mixer::mix_graph(project, q, pos, n, sources, live), Layout::Stereo, mixdown);
                    for c in 0..2 {
                        out.channels[c][off..off + n].copy_from_slice(&b.channels[c.min(b.channels.len() - 1)][..n]);
                    }
                }
            }
            pos += n as i64;
        }
        out
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// The matte cache whose epochs salt the hashes of segments with Magic Mask nodes.
    pub fn set_mattes(&self, cache: Arc<crate::magic_mask::MatteCache>) {
        *self.mattes.write().unwrap_or_else(|e| e.into_inner()) = Some(cache);
    }

    /// Segments of `seq` in this project snapshot (memoized per snapshot and matte epoch).
    pub fn segments(&self, project: &Arc<Project>, seq: ItemId) -> Arc<Vec<Segment>> {
        let mattes = self.mattes.read().unwrap_or_else(|e| e.into_inner()).clone();
        let epoch = mattes.as_ref().map_or(0, |m| m.epoch());
        let mut memo = self.memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, _, s)) = memo.iter().find(|(p, q, e, _)| Arc::ptr_eq(p, project) && *q == seq && *e == epoch) {
            return s.clone();
        }
        let mut segs = video_segments(project, seq);
        salt_magic_masks(&mut segs, project, seq, mattes.as_deref());
        let segs = Arc::new(segs);
        memo.push((project.clone(), seq, epoch, segs.clone()));
        if memo.len() > 4 {
            memo.remove(0);
        }
        segs
    }

    /// Green when previews cover the whole segment, else its colour without a preview.
    pub fn state_of(&self, seg: &Segment) -> BarState {
        if self.is_rendered(seg) { BarState::Green } else { need_state(seg) }
    }

    /// The render bar: per segment, green over its rendered frames and its own colour elsewhere;
    /// adjacent spans of the same colour merged.
    pub fn bar(&self, project: &Arc<Project>, seq: ItemId) -> Vec<BarSpan> {
        let Some(rate) = project.sequence(seq).map(|q| q.settings.frame_rate) else { return Vec::new() };
        let mut out: Vec<BarSpan> = Vec::new();
        let building = self.cache.building();
        for s in self.segments(project, seq).iter() {
            let need = need_state(s);
            let mut spans = Vec::new();
            let mut pos = 0;
            // the part the render cache is building (its frames not rendered yet) shows as such
            let unrendered = |spans: &mut Vec<(i64, i64, BarState)>, a: i64, b: i64| match &building {
                Some((h, off, len)) if *h == s.hash => {
                    let (x, y) = ((*off).clamp(a, b), off.saturating_add(*len).clamp(a, b));
                    for (p, q, st) in [(a, x, need), (x, y, BarState::Building), (y, b, need)] {
                        if p < q {
                            spans.push((p, q, st));
                        }
                    }
                }
                _ => spans.push((a, b, need)),
            };
            for (a, b) in self.coverage(s) {
                if a > pos {
                    unrendered(&mut spans, pos, a);
                }
                spans.push((a, b, BarState::Green));
                pos = b;
            }
            if pos < s.frames {
                unrendered(&mut spans, pos, s.frames);
            }
            let at = |f: i64| match f {
                0 => s.start,
                f if f >= s.frames => s.end,
                f => rate.tick_of(s.first_frame.saturating_add(f)),
            };
            for (a, b, state) in spans {
                let (start, end) = (at(a), at(b));
                match out.last_mut() {
                    Some(l) if l.end == start && l.state == state => l.end = end,
                    _ => out.push(BarSpan { start, end, state }),
                }
            }
        }
        out
    }

    /// The preview frame for sequence frame `frame`, if its segment has been rendered.
    pub fn frame(&self, pool: &MediaPool, project: &Arc<Project>, seq: ItemId, frame: i64, scale: f32) -> Option<Arc<VideoFrame>> {
        if self.files.read().unwrap_or_else(|e| e.into_inner()).is_empty() {
            return None;
        }
        let segs = self.segments(project, seq);
        let seg = segment_at(&segs, frame)?;
        let rel = frame.checked_sub(seg.first_frame)?;
        let (name, off) = self.part_at(&seg.hash, rel)?;
        let src = self.open(pool, &name)?;
        self.cache.touch(&name);
        let rate = project.sequence(seq)?.settings.frame_rate;
        src.video_frame(FrameRequest { time: rate.tick_of(rel.saturating_sub(off)), scale }).ok()
    }

    /// The preview file holding frame `rel` (from the segment start) of segment `hash`, and the
    /// file's first frame. The latest-starting file wins where files overlap (the render cache's
    /// parts start at every 2 s, a partial render may start anywhere).
    fn part_at(&self, hash: &str, rel: i64) -> Option<(String, i64)> {
        let video = self.video.read().unwrap_or_else(|e| e.into_inner());
        video.get(hash)?.iter().filter(|p| p.off <= rel && rel < p.off.saturating_add(p.len)).max_by_key(|p| p.off).map(|p| (p.name.clone(), p.off))
    }

    /// The preview file playback needs after sequence frame `frame`, when `frame` is within
    /// [`PREOPEN_FRAMES`] of the end of its file (of its segment when it plays live): the next part
    /// of the segment, else the first file of the next segment. None when there is none, or it is
    /// not near.
    pub fn next_part(&self, project: &Arc<Project>, seq: ItemId, frame: i64) -> Option<String> {
        if self.files.read().unwrap_or_else(|e| e.into_inner()).is_empty() {
            return None;
        }
        let segs = self.segments(project, seq);
        let i = segs.partition_point(|g| g.first_frame <= frame).checked_sub(1)?;
        let seg = segs.get(i).filter(|g| g.contains_frame(frame))?;
        let rel = frame.saturating_sub(seg.first_frame);
        // the end of the file on screen, else (a live frame) the end of the segment
        let end = match self.part_at(&seg.hash, rel) {
            Some((_, off)) => {
                let video = self.video.read().unwrap_or_else(|e| e.into_inner());
                video.get(&seg.hash)?.iter().filter(|p| p.off == off).map(|p| p.off.saturating_add(p.len)).max()?.min(seg.frames)
            }
            None => seg.frames,
        };
        if end.saturating_sub(rel) > PREOPEN_FRAMES {
            return None;
        }
        if end < seg.frames {
            return self.part_at(&seg.hash, end).map(|(n, _)| n);
        }
        // the next segment, when it starts where this one ends
        let next = segs.get(i.saturating_add(1)).filter(|g| g.first_frame == seg.first_frame.saturating_add(seg.frames))?;
        self.part_at(&next.hash, 0).map(|(n, _)| n)
    }

    /// Open the preview file playback needs after `frame` ahead of time ([`Self::next_part`]):
    /// prefetch calls this so a cut or a part boundary does not wait for the file to open.
    /// Returns whether a file was opened now.
    pub fn preopen_next(&self, pool: &MediaPool, project: &Arc<Project>, seq: ItemId, frame: i64) -> bool {
        let Some(name) = self.next_part(project, seq, frame) else { return false };
        if self.is_open(&name) {
            return false;
        }
        self.open(pool, &name).is_some()
    }

    /// Whether preview file `name` is open.
    pub fn is_open(&self, name: &str) -> bool {
        self.sources.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(n, _)| n == name)
    }

    /// The opened preview file `name` (`<hash>.mov` or a part).
    fn open(&self, pool: &MediaPool, name: &str) -> Option<SharedSource> {
        {
            let mut g = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = g.iter().position(|(n, _)| n == name) {
                let e = g.remove(i);
                let s = e.1.clone();
                g.push(e);
                return Some(s);
            }
        }
        let path = self.dir()?.join(name);
        let src = open_part_file(pool, name, &path)?;
        let mut g = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = g.iter().position(|(n, _)| n == name) {
            // opened by another thread meanwhile
            return g.get(i).map(|(_, s)| s.clone());
        }
        g.push((name.to_string(), src.clone()));
        while g.len() > MAX_OPEN_PARTS {
            g.remove(0);
        }
        Some(src)
    }
}

/// Open a preview file in place: containers read their index now and samples on demand, so
/// opening costs the index, not the file (fork 6.3; the store used to read whole files). A file no
/// streaming reader takes is read whole only up to [`MAX_WHOLE_READ`] bytes.
#[cfg(any(unix, windows))]
fn open_part_file(pool: &MediaPool, name: &str, path: &Path) -> Option<SharedSource> {
    let reader = filmcraft_media::reader::FileReader::open(path).ok()?;
    open_part_reader(pool, name, Arc::new(reader))
}

#[cfg(not(any(unix, windows)))]
fn open_part_file(pool: &MediaPool, name: &str, path: &Path) -> Option<SharedSource> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_WHOLE_READ {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    pool.open_bytes(name, bytes.into()).ok()
}

/// Largest `moov` box of a preview file read (an hour-long whole-segment preview's is well under
/// 1 MB; the demuxer itself accepts up to 1 GB).
const MAX_PREVIEW_MOOV: u64 = 32 << 20;

/// Whether a QuickTime / MP4 file's top-level boxes announce a `moov` larger than
/// [`MAX_PREVIEW_MOOV`] (a corrupt or foreign file): reads 16 bytes per top-level box, at most 64
/// boxes.
fn moov_too_large(r: &dyn filmcraft_media::ByteReader) -> bool {
    let len = r.len();
    let mut off = 0u64;
    for _ in 0..64 {
        if off.saturating_add(8) > len {
            return false;
        }
        let mut h = [0u8; 16];
        let n = len.saturating_sub(off).min(16) as usize;
        let Some(buf) = h.get_mut(..n) else { return false };
        if r.read_at(off, buf).is_err() {
            return false;
        }
        let size32 = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let size = match size32 {
            0 => len.saturating_sub(off),
            1 if n >= 16 => u64::from_be_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]]),
            s if s >= 8 => s,
            _ => return false,
        };
        if &h[4..8] == b"moov" {
            return size > MAX_PREVIEW_MOOV;
        }
        if size32 == 0 {
            return false;
        }
        off = off.saturating_add(size.max(8));
    }
    false
}

/// [`open_part_file`] from a reader (tests count what it reads).
pub(crate) fn open_part_reader(pool: &MediaPool, name: &str, reader: filmcraft_media::SharedReader) -> Option<SharedSource> {
    if moov_too_large(&*reader) {
        log::warn!("preview {name}: its index is too large (a damaged file); not opened");
        return None;
    }
    let openers = pool.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
    filmcraft_media::reader::open_reader_within(name, reader, &filmcraft_codecs::reader_openers(), &openers, MAX_WHOLE_READ).ok()
}

fn need_state(seg: &Segment) -> BarState {
    match seg.need {
        Need::None => BarState::None,
        Need::Realtime => BarState::Yellow,
        Need::Render => BarState::Red,
    }
}

/// Fold the matte epochs of the Magic Mask nodes a segment shows into its hash (fork G3.2;
/// segments without one keep theirs). Clips inside nested sequences are not followed.
fn salt_magic_masks(segs: &mut [Segment], project: &Project, seq: ItemId, mattes: Option<&crate::magic_mask::MatteCache>) {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let Some(q) = project.sequence(seq) else { return };
    for seg in segs.iter_mut() {
        let mut keys: Vec<(u64, u64)> = Vec::new();
        for c in &seg.clips {
            if let Some((_, it)) = q.find_item(*c)
                && let Some(g) = &it.grade
            {
                keys.extend(g.nodes.iter().filter(|n| n.magic_mask.is_some()).map(|n| (c.0, n.id)));
            }
        }
        if keys.is_empty() {
            continue;
        }
        let mut parts = [0u64; 2];
        for (i, part) in parts.iter_mut().enumerate() {
            let mut h = DefaultHasher::new();
            (i, "magic-mask", &seg.hash).hash(&mut h);
            for k in &keys {
                (k, mattes.map_or(0, |m| m.matte_epoch(ClipId(k.0), k.1))).hash(&mut h);
            }
            *part = h.finish();
        }
        seg.hash = format!("{:016x}{:016x}", parts[0], parts[1]);
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn default_temp_dir() -> Option<PathBuf> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    Some(untitled_dir(&crate::temp_dir().join("FilmCraft Previews")))
}

/// A fresh per-process folder for an unsaved project's previews under `root`.
fn untitled_dir(root: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    root.join(format!("untitled-{}-{nanos:x}", std::process::id()))
}

/// The preview folder of a project saved at `project_path`.
pub fn dir_for_project(project_path: &str) -> PathBuf {
    let p = Path::new(project_path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into());
    p.parent().unwrap_or(Path::new(".")).join("FilmCraft Previews").join(stem)
}

// ---------------------------------------------------------------- commands

/// In/Out of the active sequence (whole sequence when neither is set).
fn in_out(s: &Session) -> Result<(ItemId, TimeRange)> {
    let id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.project.sequence(id).ok_or(EngineError::NoSequence)?;
    let fd = q.settings.frame_rate.frame_duration();
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let b = q.mark_out.map(|o| o + fd).unwrap_or(q.duration());
    Ok((id, TimeRange::from_bounds(a, b.max(a))))
}

/// One preview file to render: frames `off..off + len` of segment `seg` (from its start).
struct Todo {
    seg: Segment,
    off: i64,
    len: i64,
}

/// Start rendering previews as a background job. `{"wait": true}` renders synchronously.
///
/// The In/Out modes render only the frames between In and Out (a segment that extends past them
/// gets a partial preview), and every mode renders only frames that have no preview yet.
pub fn render(s: &mut Session, mode: RenderMode, p: &Value) -> Result<Value> {
    let (seq, range) = in_out(s)?;
    let rate = s.project.sequence(seq).ok_or(EngineError::NoSequence)?.settings.frame_rate;
    let store = s.previews.clone();
    let dir = store.dir().ok_or_else(|| EngineError::Other("render previews are not available here (no preview folder)".into()))?;
    let segs = store.segments(&s.project, seq);
    let selection: HashSet<_> = s.state.selection.iter().copied().collect();
    let mut todo: Vec<Todo> = Vec::new();
    for g in segs.iter() {
        let (a, b) = match mode {
            RenderMode::EffectsInToOut if g.need == Need::None => continue,
            RenderMode::EffectsInToOut | RenderMode::InToOut => rel_range(g, rate, &range),
            RenderMode::Selection if g.clips.iter().any(|c| selection.contains(c)) => (0, g.frames.max(0)),
            RenderMode::Selection => continue,
        };
        for (off, end) in gaps(&store.coverage(g), a, b) {
            todo.push(Todo { seg: g.clone(), off, len: end - off });
        }
    }
    if todo.is_empty() {
        s.toast("Nothing to render: previews are up to date");
        return Ok(json!({"job": null, "segments": 0}));
    }
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("preview folder {}: {e}", dir.display())))?;
    let frames: i64 = todo.iter().map(|t| t.len).fold(0, i64::saturating_add);
    let id = s.jobs.len() as u64 + 1;
    let job = crate::Job {
        id,
        label: format!("Rendering {} preview segment{}", todo.len(), if todo.len() == 1 { "" } else { "s" }),
        progress: Default::default(),
        result: Default::default(),
    };
    job.progress.total.store(frames.max(1) as u64, Ordering::Relaxed);
    let project = s.project.clone();
    // Previews are cached by content and reused with proxies on or off: always full resolution.
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let nseg = todo.len();
    let mattes = s.magic_mask.token();
    let pool = s.media.clone();
    let run = move || {
        let t0 = web_time::Instant::now();
        let mut bytes = 0u64;
        let mut done_frames = 0u64;
        let mut outcome: std::result::Result<(), String> = Ok(());
        for (k, t) in todo.iter().enumerate() {
            if prog.cancel.load(Ordering::Relaxed) {
                outcome = Err("cancelled".into());
                break;
            }
            *prog.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("Rendering segment {} of {nseg}", k + 1);
            let name = part_name(&t.seg.hash, t.off, t.len, t.seg.frames);
            let part = dir.join(format!("{name}.part"));
            let first = t.seg.first_frame.saturating_add(t.off);
            let settings = filmcraft_export::ExportSettings {
                format: filmcraft_export::Format::ProRes,
                path: part.to_string_lossy().to_string(),
                range: Some(TimeRange::from_bounds(rate.tick_of(first), rate.tick_of(first.saturating_add(t.len)))),
                scale: 1.0,
                include_audio: false,
                quality: 90,
                bitrate_kbps: 0,
                // Captions stay live over previews and are not part of the preview hash.
                burn_captions: false,
                part_of_batch: true,
                // previews stand in for the monitor picture: display-referred SDR
                sdr: true,
                // Magic Mask mattes tracked by now (fork G3.2)
                mattes,
                ..Default::default()
            };
            match filmcraft_export::export(&project, seq, &settings, &provider, &prog) {
                Ok(r) => {
                    bytes += r.bytes;
                    done_frames += r.frames;
                    let fin = dir.join(&name);
                    if let Err(e) = std::fs::rename(&part, &fin) {
                        outcome = Err(e.to_string());
                        break;
                    }
                    store.add_video(&name);
                    // open it now so the first playback doesn't wait for the file read
                    let _ = store.open(&pool, &name);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    outcome = Err(e.to_string());
                    break;
                }
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        let r = outcome.map(|_| filmcraft_export::Report {
            path: dir.to_string_lossy().to_string(),
            frames: done_frames,
            seconds: secs,
            bytes,
            render_fps: done_frames as f64 / secs.max(1e-6),
            extra_files: Vec::new(),
        });
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
        }
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) =
            if r.is_ok() { format!("Rendered {done_frames} frames in {secs:.1}s") } else { "Render stopped".into() };
        prog.finished.store(true, Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    if wait || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-render-previews".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(json!({"job": id, "segments": nseg, "frames": frames}))
}

/// Render Audio: mix the audio segments in In/Out (or the whole sequence) to float WAV previews.
pub fn render_audio(s: &mut Session, p: &Value) -> Result<Value> {
    let (seq, range) = in_out(s)?;
    let store = s.previews.clone();
    let dir = store.dir().ok_or_else(|| EngineError::Other("render previews are not available here (no preview folder)".into()))?;
    let sr = s.project.sequence(seq).map(|q| q.settings.sample_rate).unwrap_or(48_000) as i64;
    let (a, b) = (range.start.to_units_floor(sr), range.end().to_units_floor(sr));
    let todo: Vec<AudioSegment> = store
        .audio_segments(&s.project, seq)
        .iter()
        .filter(|g| g.first_sample < b && a < g.first_sample + g.samples && !store.has_audio(&g.hash))
        .cloned()
        .collect();
    if todo.is_empty() {
        s.toast("Nothing to render: audio previews are up to date");
        return Ok(json!({"job": null, "segments": 0}));
    }
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Other(format!("preview folder {}: {e}", dir.display())))?;
    let total: i64 = todo.iter().map(|g| g.samples).sum();
    let id = s.jobs.len() as u64 + 1;
    let job = crate::Job { id, label: "Rendering audio previews".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(total.max(1) as u64, Ordering::Relaxed);
    let project = s.project.clone();
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    let (prog, res) = (job.progress.clone(), job.result.clone());
    let nseg = todo.len();
    let run = move || {
        let t0 = web_time::Instant::now();
        let mut bytes = 0u64;
        let mut outcome: std::result::Result<(), String> = Ok(());
        let Some(q) = project.sequence(seq) else { return };
        'segs: for g in &todo {
            let mut inter = Vec::with_capacity(g.samples as usize * 2);
            let mut pos = g.first_sample;
            while pos < g.first_sample + g.samples {
                if prog.cancel.load(Ordering::Relaxed) {
                    outcome = Err("cancelled".into());
                    break 'segs;
                }
                let n = (g.first_sample + g.samples - pos).min(sr) as usize;
                let buf = filmcraft_render::audio::mix_sequence(&project, q, pos, n, &provider);
                for i in 0..n {
                    inter.push(buf.channels[0][i]);
                    inter.push(buf.channels[buf.channels.len().min(2) - 1][i]);
                }
                pos += n as i64;
                prog.done.fetch_add(n as u64, Ordering::Relaxed);
            }
            let data = write_wav_f32(&inter, sr as u32);
            let part = dir.join(format!("{}.wav.part", g.hash));
            let fin = dir.join(audio_name(&g.hash));
            if let Err(e) = std::fs::write(&part, &data).and_then(|_| std::fs::rename(&part, &fin)) {
                let _ = std::fs::remove_file(&part);
                outcome = Err(e.to_string());
                break;
            }
            bytes += data.len() as u64;
            store.add_audio(&g.hash);
        }
        let secs = t0.elapsed().as_secs_f64();
        let r = outcome.map(|_| filmcraft_export::Report {
            path: dir.to_string_lossy().to_string(),
            frames: 0,
            seconds: secs,
            bytes,
            render_fps: 0.0,
            extra_files: Vec::new(),
        });
        if let Err(e) = &r {
            *prog.error.lock().unwrap_or_else(|x| x.into_inner()) = Some(e.clone());
        }
        *prog.status.lock().unwrap_or_else(|e| e.into_inner()) =
            if r.is_ok() { format!("Rendered audio for {nseg} segment(s) in {secs:.1}s") } else { "Render stopped".into() };
        prog.finished.store(true, Ordering::Relaxed);
        *res.lock().unwrap_or_else(|x| x.into_inner()) = Some(r);
    };
    let run = crate::export_tools::guard_job(job.progress.clone(), job.result.clone(), run);
    s.jobs.push(job);
    if p.get("wait").and_then(Value::as_bool).unwrap_or(false) || cfg!(target_arch = "wasm32") {
        run();
    } else {
        std::thread::Builder::new().name("filmcraft-render-audio".into()).spawn(run).map_err(|e| EngineError::Other(e.to_string()))?;
    }
    Ok(json!({"job": id, "segments": nseg, "samples": total}))
}

/// 32-bit float stereo WAV (WAVE_FORMAT_IEEE_FLOAT) from interleaved samples.
pub fn write_wav_f32(interleaved: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (interleaved.len() * 4) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 8).to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in interleaved {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Read the interleaved stereo samples of a file written by [`write_wav_f32`].
pub fn read_wav_f32(b: &[u8]) -> Option<Vec<f32>> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    let mut float_stereo = false;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().ok()?) as usize;
        let body = b.get(i + 8..(i + 8 + len).min(b.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            let fmt = u16::from_le_bytes([body[0], body[1]]);
            let ch = u16::from_le_bytes([body[2], body[3]]);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            float_stereo = fmt == 3 && ch == 2 && bits == 32;
        } else if id == b"data" {
            if !float_stereo {
                return None;
            }
            return Some(body.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect());
        }
        i += 8 + len + (len & 1);
    }
    None
}
/// Delete Render Files (all of the project's previews) or Delete Render Files In to Out.
pub fn delete(s: &mut Session, in_to_out: bool) -> Result<Value> {
    let n = if in_to_out {
        let (seq, range) = in_out(s)?;
        let rate = s.project.sequence(seq).ok_or(EngineError::NoSequence)?.settings.frame_rate;
        // the files holding frames between In and Out (a partial preview outside them stays)
        let names: Vec<String> = s
            .previews
            .segments(&s.project, seq)
            .iter()
            .flat_map(|g| {
                let (a, b) = rel_range(g, rate, &range);
                s.previews.parts_in(g, a, b)
            })
            .collect();
        s.previews.delete_files(&names)
    } else {
        s.previews.delete(None)
    };
    s.toast(format!("Deleted {n} render file{}", if n == 1 { "" } else { "s" }));
    Ok(json!({"deleted": n}))
}

/// The render bar of the active sequence as JSON (for agents and tests).
pub fn bar_json(s: &Session) -> Result<Value> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let rate = s.sequence_rate();
    let segs = s.previews.segments(&s.project, seq);
    Ok(json!({
        "dir": s.previews.dir().map(|d| d.to_string_lossy().to_string()),
        "files": s.previews.count(),
        "segments": segs.iter().map(|g| json!({
            "start": g.start.0,
            "end": g.end.0,
            "startFrame": g.first_frame,
            "frames": g.frames,
            "startSeconds": g.start.seconds(),
            "endSeconds": g.end.seconds(),
            "state": s.previews.state_of(g),
            "renderedFrames": s.previews.coverage(g).iter().map(|(a, b)| b - a).sum::<i64>(),
            "costMs": (g.cost_ms * 10.0).round() / 10.0,
            "budgetMs": (rate.frame_duration().seconds() * 1000.0 * filmcraft_render::preview::REALTIME_BUDGET * 10.0).round() / 10.0,
            "hash": g.hash,
        })).collect::<Vec<_>>(),
    }))
}
