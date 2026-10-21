//! The media pool: one shared [`MediaSource`] per project item, created lazily from its
//! [`MediaRef`], plus the registry of container/codec openers.
//!
//! - Sources are cached per item **and per reference** (the file path, the offline flag), so a
//!   relink, Make Offline or their undo picks up the right file on the next frame.
//! - Media that can't be opened (missing, unreadable) or was made offline renders the offline
//!   slate ([`filmcraft_render::offline`]) instead of failing; the reason is kept for the UI. So
//!   do the frames of a file that opens but whose picture FilmCraft cannot decode (a codec or
//!   profile without a decoder).
//! - Which frames read attached proxies follows the [`ProxyMode`] (Settings ▸ Media ▸ Proxies):
//!   **On** every frame but exports, **Off** none, **Auto** (the default) every frame while the
//!   user plays or scrubs ([`MediaPool::set_motion`], set from the background worker's activity
//!   each UI frame) and none while paused; stopping redraws the paused frame from the original.
//!   A per-frame hint ([`with_motion`]) overrides that, but nothing in the product sets it yet
//!   (owed: the frame server sets it per frame job, so prefetch of paused frames reads originals). Items with an attached proxy are then read from it. Proxy frames are smaller; the source still reports the full-resolution size, and the
//!   compositor derives its pixel scale from the frame it gets, so effects and Motion render the
//!   same picture at lower resolution. Export asks for [`MediaPool::full_res_provider`].

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, Opener, SharedSource};
use filmcraft_project::{ItemId, MediaClip, MediaRef, Project};
use filmcraft_render::offline::OfflineReason;

use crate::Services;

/// Which frames read attached proxies (Settings ▸ Media ▸ Proxies, `media.proxyMode`). Exports
/// always read the originals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyMode {
    /// Playback, prefetch and scrubbing read proxies; a paused frame reads the original.
    #[default]
    Auto,
    /// Every frame reads the proxy (Toggle Proxies on).
    On,
    /// No frame reads a proxy.
    Off,
}

impl ProxyMode {
    pub const ALL: [Self; 3] = [Self::Auto, Self::On, Self::Off];

    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id().eq_ignore_ascii_case(s.trim()))
    }

    /// Whether a frame reads an attached proxy. `motion`: the frame is for playback, prefetch or
    /// scrubbing (`Some(true)`), shown while paused (`Some(false)`), or unknown (`None`: treated
    /// as paused, so a caller that says nothing never gets a lower-resolution frame).
    pub fn use_proxy(self, motion: Option<bool>) -> bool {
        match self {
            Self::On => true,
            Self::Off => false,
            Self::Auto => motion.unwrap_or(false),
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::On => 1,
            Self::Off => 2,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::On,
            2 => Self::Off,
            _ => Self::Auto,
        }
    }
}

/// Reads `"auto" | "on" | "off"`, and the `true` / `false` of the older `enableProxies` switch
/// (`true` = On; `false` = Auto, the new default: proxies were off only because there were none).
impl<'de> serde::Deserialize<'de> for ProxyMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Str(String),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Bool(true) => Self::On,
            Raw::Bool(false) => Self::Auto,
            Raw::Str(s) => Self::parse(&s).unwrap_or_default(),
        })
    }
}

thread_local! {
    static MOTION: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Run `f` with the frame-use hint `motion` (restoring the previous one after): the frames it
/// renders are for playback, prefetch or scrubbing (`true`) or shown while paused (`false`). In
/// [`ProxyMode::Auto`] only the former read proxies. Not set by the product yet (owed: the frame
/// server sets it per frame job); without it Auto follows [`MediaPool::set_motion`].
pub fn with_motion<R>(motion: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            MOTION.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(MOTION.with(|c| c.replace(Some(motion))));
    f()
}

/// The frame-use hint of the work running on this thread ([`with_motion`]; None: not said).
pub fn motion() -> Option<bool> {
    MOTION.with(Cell::get)
}

/// Why an item renders the offline slate, as last seen by the pool.
#[derive(Clone, Debug, PartialEq)]
pub struct OfflineStatus {
    pub reason: OfflineReason,
    pub path: String,
    pub error: String,
}

pub struct MediaPool {
    /// item → (reference key, source)
    sources: RwLock<HashMap<ItemId, (String, SharedSource)>>,
    proxies: RwLock<HashMap<ItemId, (String, SharedSource)>>,
    offline: Arc<RwLock<HashMap<ItemId, OfflineStatus>>>,
    /// [`ProxyMode`] as a byte.
    proxy_mode: AtomicU8,
    /// The user plays or scrubs ([`MediaPool::set_motion`]): what [`ProxyMode::Auto`] goes by for
    /// frames rendered without a [`with_motion`] hint.
    motion: AtomicBool,
    /// Openers tried before the built-in ones (MP4/MOV + codecs register here).
    pub openers: RwLock<Vec<Opener>>,
    /// Hand-off scales of an export's lane copy ([`MediaPool::lane_copy_scaled`]; empty: full
    /// size), given to each source when it is opened.
    handoff: Vec<(ItemId, f32)>,
    /// Local copies of share clips (STORAGE-PLAN S1 item 2; `None`: off). A complete copy is
    /// opened instead of the share.
    net_copies: Arc<RwLock<Option<Arc<std::sync::Mutex<crate::net_cache::NetCache>>>>>,
}

impl Default for MediaPool {
    /// A pool with the built-in container/codec openers registered.
    fn default() -> Self {
        Self {
            sources: RwLock::new(HashMap::new()),
            proxies: RwLock::new(HashMap::new()),
            offline: Arc::new(RwLock::new(HashMap::new())),
            proxy_mode: AtomicU8::new(ProxyMode::default().to_u8()),
            motion: AtomicBool::new(false),
            openers: RwLock::new(filmcraft_codecs::openers()),
            handoff: Vec::new(),
            net_copies: Arc::default(),
        }
    }
}

/// Largest file [`MediaPool::probe_file`] reads whole when no streaming reader takes it (WAV,
/// MP3 and other byte-opened formats, or an unsupported container such as AVI).
pub const PROBE_WHOLE_FILE_MAX: u64 = 64 * 1024 * 1024;

/// Cache key of a media clip's full-resolution reference ("" matches anything: sources inserted
/// without a key). A generator's key holds its parameters, so a Color Matte whose color changes
/// (or comes back with undo) is generated again.
pub fn media_key(m: &MediaClip) -> String {
    let key = match &m.media {
        MediaRef::File { path } if m.offline => format!("offline:{path}"),
        MediaRef::File { path } => format!("file:{path}"),
        MediaRef::Generator(g) => format!("generator:{g:?}"),
    };
    // an interpreted frame rate wraps the file's source ([`InterpretedRate`])
    match m.interpret.frame_rate {
        Some(r) => format!("{key}@{}/{}", r.num, r.den),
        None => key,
    }
}

/// A movie interpreted at a constant frame rate (Y2: Interpret as CFR / Conform to Sequence
/// Rate, Interpret Footage's `frameRate`): its `k`-th frame is shown at `k / rate`, whatever the
/// file's own frame times say, and it lasts its frames at that rate. Its sound plays as recorded
/// (provisional, owner question 62), so on a long variable-frame-rate clip it drifts from the
/// picture: the trade-off of playing every frame evenly.
pub struct InterpretedRate {
    inner: SharedSource,
    info: MediaInfo,
    rate: filmcraft_time::FrameRate,
}

/// `src` wrapped in [`InterpretedRate`] when `m` is a movie with an interpreted frame rate.
fn interpreted(m: &MediaClip, src: SharedSource) -> SharedSource {
    let Some(rate) = m.interpret.frame_rate.filter(|r| r.num > 0 && r.den > 0 && r.frame_duration().0 > 0) else { return src };
    if m.info.kind != filmcraft_media::MediaKind::Movie || m.info.video.is_none() {
        return src;
    }
    let mut info = src.info().clone();
    info.duration = m.duration();
    if let Some(v) = info.video.as_mut() {
        v.frame_rate = rate;
        v.vfr = None;
    }
    Arc::new(InterpretedRate { inner: src, info, rate })
}

impl MediaSource for InterpretedRate {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn set_handoff_scale(&self, scale: f32) {
        self.inner.set_handoff_scale(scale);
    }
    fn decoder_name(&self) -> Option<String> {
        self.inner.decoder_name()
    }
    fn frame_clock(&self) -> filmcraft_media::timing::FrameClock {
        filmcraft_media::timing::FrameClock::Grid(self.rate)
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        use filmcraft_media::timing::FrameTimes;
        let k = self.rate.frame_at(req.time.max(filmcraft_time::Tick::ZERO));
        let t = self.inner.frame_clock().tick_of(k);
        self.inner.video_frame(FrameRequest { time: t, scale: req.scale })
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio_stream(stream, start, frames, sample_rate)
    }
}

/// Open the image sequence that `first` (a numbered still) starts: its frames from that number to
/// the highest present one, played at `rate`. Frames are listed with [`Services::list_dir`], or
/// found by probing consecutive numbers (stopping after 100 missing in a row) when the host cannot
/// list directories.
pub fn open_image_sequence(first: &str, services: &dyn Services, rate: filmcraft_time::FrameRate, name: &str) -> Result<SharedSource, MediaError> {
    let frames = image_sequence_frames(first, services)?;
    let loader = match services.file_loader() {
        Some(l) => l,
        None => {
            // no lasting file access: read the (encoded) frames now
            let mut files = HashMap::new();
            for p in frames.iter().flatten() {
                if let Ok(b) = services.read_file(p) {
                    files.insert(p.clone(), Arc::<[u8]>::from(b));
                }
            }
            Arc::new(move |p: &str| files.get(p).map(|b| b.to_vec()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, p.to_string())))
        }
    };
    Ok(Arc::new(filmcraft_media::sequence::ImageSequenceSource::new(name, frames, rate, loader)?))
}

/// The frames of the image sequence starting at `first` (see [`open_image_sequence`]).
pub fn image_sequence_frames(first: &str, services: &dyn Services) -> Result<Vec<Option<String>>, MediaError> {
    use filmcraft_media::sequence::{Numbered, sequence_frames};
    let n = Numbered::parse(first).ok_or_else(|| MediaError::Unsupported(format!("{first}: not a numbered still image")))?;
    let io = |e: std::io::Error| {
        if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::TimedOut) {
            MediaError::Offline(format!("{first}: {e}"))
        } else {
            MediaError::Io(format!("{first}: {e}"))
        }
    };
    services.file_size(first).map_err(io)?;
    match services.list_dir(n.dir.trim_end_matches(['/', '\\'])) {
        Some(list) => Ok(sequence_frames(&n, &list.map_err(io)?)),
        None => {
            let mut names = vec![n.file_name(n.number)];
            let mut misses = 0;
            let mut k = n.number + 1;
            while misses < 100 && names.len() < 1_000_000 {
                if services.file_size(&n.path(k)).is_ok() {
                    names.push(n.file_name(k));
                    misses = 0;
                } else {
                    misses += 1;
                }
                k += 1;
            }
            Ok(sequence_frames(&n, &names))
        }
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

impl MediaPool {
    pub fn register_opener(&self, o: Opener) {
        self.openers.write().unwrap_or_else(|e| e.into_inner()).push(o);
    }

    /// Cache a source for an item, valid whatever the item's reference (generators).
    pub fn insert(&self, item: ItemId, src: SharedSource) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (String::new(), src));
    }

    /// Cache a source for an item under its [`media_key`].
    pub fn insert_keyed(&self, item: ItemId, key: String, src: SharedSource) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (key, src));
    }

    /// Cache a source opened from `path` for an item.
    pub fn insert_file(&self, item: ItemId, path: &str, src: SharedSource) {
        self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        let src = self.unsupported_as_slate(item, path, src);
        self.sources.write().unwrap_or_else(|e| e.into_inner()).insert(item, (format!("file:{path}"), src));
    }

    /// `src` (opened from `path` for `item`) showing the unreadable slate for the frames it
    /// cannot decode.
    fn unsupported_as_slate(&self, item: ItemId, path: &str, src: SharedSource) -> SharedSource {
        let slate = SlateSource::new(src.info().clone(), &file_name(path), OfflineReason::Unreadable);
        Arc::new(UnsupportedAsSlate { inner: src, slate, item, path: path.to_string(), offline: self.offline.clone() })
    }

    pub fn remove(&self, item: ItemId) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        self.proxies.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
        self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
    }

    /// Forget every opened source (files are re-read on next use).
    pub fn clear(&self) {
        self.sources.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.proxies.write().unwrap_or_else(|e| e.into_inner()).clear();
        self.offline.write().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn cached(&self, item: ItemId) -> Option<SharedSource> {
        self.sources.read().unwrap_or_else(|e| e.into_inner()).get(&item).map(|(_, s)| s.clone())
    }

    /// Sources opened so far (originals and proxies; `perf.stats`).
    pub fn open_sources(&self) -> usize {
        self.sources.read().unwrap_or_else(|e| e.into_inner()).len() + self.proxies.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Proxies are on for every frame ([`ProxyMode::On`]: Toggle Proxies is lit).
    pub fn use_proxies(&self) -> bool {
        self.proxy_mode() == ProxyMode::On
    }

    /// Toggle Proxies: [`ProxyMode::On`] or [`ProxyMode::Off`].
    pub fn set_use_proxies(&self, on: bool) {
        self.set_proxy_mode(if on { ProxyMode::On } else { ProxyMode::Off });
    }

    pub fn proxy_mode(&self) -> ProxyMode {
        ProxyMode::from_u8(self.proxy_mode.load(Ordering::Relaxed))
    }

    pub fn set_proxy_mode(&self, m: ProxyMode) {
        self.proxy_mode.store(m.to_u8(), Ordering::Relaxed);
    }

    /// Whether a frame rendered on this thread now reads attached proxies (the mode and the
    /// [`with_motion`] hint).
    pub fn proxies_now(&self) -> bool {
        self.proxy_mode().use_proxy(Some(motion().unwrap_or_else(|| self.motion.load(Ordering::Relaxed))))
    }

    /// Whether the user plays or scrubs now (the session sets it from the background worker's
    /// [`crate::background::Activity`] every UI frame). Returns whether it changed.
    pub fn set_motion(&self, on: bool) -> bool {
        self.motion.swap(on, Ordering::Relaxed) != on
    }

    pub fn in_motion(&self) -> bool {
        self.motion.load(Ordering::Relaxed)
    }

    /// Why an item last rendered offline (None = online or not opened yet).
    pub fn offline_status(&self, item: ItemId) -> Option<OfflineStatus> {
        self.offline.read().unwrap_or_else(|e| e.into_inner()).get(&item).cloned()
    }

    /// Open a file through the registered openers.
    pub fn open_bytes(&self, name: &str, bytes: Arc<[u8]>) -> Result<SharedSource, MediaError> {
        let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
        filmcraft_media::open_bytes(name, bytes, &openers)
    }

    /// Turns local copies of share clips on (`Some`) or off (STORAGE-PLAN S1 item 2).
    pub fn set_net_copies(&self, cache: Option<Arc<std::sync::Mutex<crate::net_cache::NetCache>>>) {
        *self.net_copies.write().unwrap_or_else(|e| e.into_inner()) = cache;
    }

    /// The local copies, when they are on.
    pub fn net_copies(&self) -> Option<Arc<std::sync::Mutex<crate::net_cache::NetCache>>> {
        self.net_copies.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The complete local copy to read instead of `path` (a file on a share): the copy of the
    /// file's current version, or, when the share doesn't answer, of the last version copied.
    fn local_copy(&self, path: &str, services: &dyn Services) -> Option<String> {
        let cache = self.net_copies()?;
        // no copy of this file at all: nothing to ask the share
        if !cache.lock().unwrap_or_else(|e| e.into_inner()).has_source(path) || !crate::netfs::classify(std::path::Path::new(path)).is_network() {
            return None;
        }
        // asked without the cache lock held (the share may take its time limit to answer)
        let stamp = services.file_stamp(path);
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        let found = match stamp {
            Some((size, mtime)) => c.lookup(path, size, mtime).inspect(|_| {
                c.touch(path, size, mtime);
                c.save();
            }),
            // only a share that isn't answering falls back to the last copy; a file that is gone
            // from a working share is offline like any missing file
            None if crate::netfs::share_unreachable(path) => c.latest_for(path),
            None => None,
        };
        found.map(|p| p.to_string_lossy().into_owned())
    }

    /// Read and open a file through `services` (through its random-access reader when the host
    /// has one: containers then read only their index now and samples on demand). A clip on a
    /// share with a complete local copy is read from the copy.
    pub fn open_file(&self, path: &str, services: &dyn Services) -> Result<SharedSource, MediaError> {
        let copy = self.local_copy(path, services);
        let path = copy.as_deref().unwrap_or(path);
        let io = |e: std::io::Error| {
            if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::TimedOut) {
                MediaError::Offline(e.to_string())
            } else {
                MediaError::Io(e.to_string())
            }
        };
        if let Some(r) = services.reader(path) {
            let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
            return filmcraft_media::reader::open_reader(&file_name(path), r.map_err(io)?, &filmcraft_codecs::reader_openers(), &openers);
        }
        let bytes = services.read_file(path).map_err(io)?;
        self.open_bytes(&file_name(path), bytes.into())
    }

    /// Open a file to look at it (Media Browser properties and thumbnails), not to import it: a
    /// format without a streaming reader is read whole only up to [`PROBE_WHOLE_FILE_MAX`] bytes
    /// (#157). Hosts without random-access readers open the file as [`Self::open_file`] does.
    pub fn probe_file(&self, path: &str, services: &dyn Services) -> Result<SharedSource, MediaError> {
        let Some(r) = services.reader(path) else { return self.open_file(path, services) };
        let r = r.map_err(|e| {
            if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::TimedOut) {
                MediaError::Offline(e.to_string())
            } else {
                MediaError::Io(e.to_string())
            }
        })?;
        let openers = self.openers.read().unwrap_or_else(|e| e.into_inner()).clone();
        filmcraft_media::reader::open_reader_within(&file_name(path), r, &filmcraft_codecs::reader_openers(), &openers, PROBE_WHOLE_FILE_MAX)
    }

    /// Resolve (and cache) the source for a project item, using its proxy when the
    /// [`ProxyMode`] says so for this frame ([`Self::proxies_now`]).
    pub fn source_for(&self, p: &Project, item: ItemId, services: &dyn Services) -> Option<SharedSource> {
        self.resolve(p, item, services, self.proxies_now())
    }

    /// Resolve the full-resolution source for an item (never a proxy).
    pub fn full_res_source(&self, p: &Project, item: ItemId, services: &dyn Services) -> Option<SharedSource> {
        self.resolve(p, item, services, false)
    }

    fn resolve(&self, p: &Project, item: ItemId, services: &dyn Services, proxies: bool) -> Option<SharedSource> {
        let (item, m, _) = p.resolve_media(item)?;
        let it = p.item(item)?;
        // A variable-frame-rate clip interpreted at a constant rate needs the original's own frame
        // times to number its frames ([`InterpretedRate`]); its constant-rate proxy doesn't have
        // them, so it plays from the original (a constant-rate clip's proxy has the same grid).
        let interpreted_vfr = m.interpret.frame_rate.is_some() && m.info.video.as_ref().is_some_and(|v| v.vfr.is_some());
        if proxies
            && !m.offline
            && !interpreted_vfr
            && let Some(MediaRef::File { path }) = &m.proxy
            && let Some(s) = self.proxy_source(item, path, m, services)
        {
            return Some(s);
        }
        let key = media_key(m);
        if let Some((k, s)) = self.sources.read().unwrap_or_else(|e| e.into_inner()).get(&item)
            && (k.is_empty() || *k == key)
        {
            return Some(s.clone());
        }
        let src: SharedSource = match &m.media {
            MediaRef::Generator(g) => {
                let v = m.info.video.as_ref();
                let (w, h, r) = v.map(|v| (v.width, v.height, v.frame_rate)).unwrap_or((1920, 1080, Default::default()));
                Arc::new(GeneratorSource::new(g.clone(), w, h, r, m.info.duration).with_name(&it.name))
            }
            MediaRef::File { path } if m.offline => {
                self.set_offline(item, OfflineReason::MadeOffline, path, "made offline");
                Arc::new(SlateSource::new(m.info.clone(), &file_name(path), OfflineReason::MadeOffline))
            }
            MediaRef::File { path } => match if m.info.kind == filmcraft_media::MediaKind::ImageSequence {
                open_image_sequence(path, services, m.info.frame_rate(), &it.name)
            } else {
                self.open_file(path, services)
            } {
                Ok(s) => {
                    if let Some(&(_, scale)) = self.handoff.iter().find(|h| h.0 == item) {
                        s.set_handoff_scale(scale);
                    }
                    self.offline.write().unwrap_or_else(|e| e.into_inner()).remove(&item);
                    self.unsupported_as_slate(item, path, interpreted(m, s))
                }
                Err(_) if filmcraft_media::pending::is_set() => {
                    // the host is still fetching the file's bytes: show the slate for now, retry later
                    return Some(Arc::new(SlateSource::new(m.info.clone(), &file_name(path), OfflineReason::Missing)));
                }
                Err(e) => {
                    log::warn!("media offline: {path}: {e}");
                    let reason = if matches!(e, MediaError::Offline(_)) { OfflineReason::Missing } else { OfflineReason::Unreadable };
                    self.set_offline(item, reason, path, &e.to_string());
                    Arc::new(SlateSource::new(m.info.clone(), &file_name(path), reason))
                }
            },
        };
        // Parallel render workers can all miss above and open the file at once: the first one
        // stored wins, so every worker shares one source (one decoder and one GOP cache) instead
        // of decoding the same GOP once per worker.
        let mut sources = self.sources.write().unwrap_or_else(|e| e.into_inner());
        if let Some((k, s)) = sources.get(&item)
            && (k.is_empty() || *k == key)
        {
            return Some(s.clone());
        }
        sources.insert(item, (key, src.clone()));
        Some(src)
    }

    fn set_offline(&self, item: ItemId, reason: OfflineReason, path: &str, error: &str) {
        self.offline.write().unwrap_or_else(|e| e.into_inner()).insert(item, OfflineStatus { reason, path: path.to_string(), error: error.to_string() });
    }

    fn proxy_source(&self, item: ItemId, path: &str, m: &MediaClip, services: &dyn Services) -> Option<SharedSource> {
        let key = format!("proxy:{path}{}", m.interpret.frame_rate.map(|r| format!("@{}/{}", r.num, r.den)).unwrap_or_default());
        if let Some((k, s)) = self.proxies.read().unwrap_or_else(|e| e.into_inner()).get(&item)
            && *k == key
        {
            return Some(s.clone());
        }
        match self.open_file(path, services) {
            Ok(s) => {
                // a proxy with fewer audio streams than the original (say one track of an OBS
                // recording's seven) still stands in for the picture; the audio comes from the
                // original, so every clip keeps the stream it plays
                let audio_from = if s.info().audio_streams.len() < m.info.audio_streams.len() {
                    log::warn!("proxy has fewer audio streams than the original: {path}; audio from full-resolution media");
                    match &m.media {
                        MediaRef::File { path: original } if !m.offline => self.open_file(original, services).ok(),
                        _ => None,
                    }
                } else {
                    None
                };
                let src: SharedSource = interpreted(m, Arc::new(ProxySource { proxy: s, info: m.info.clone(), audio_from }));
                self.proxies.write().unwrap_or_else(|e| e.into_inner()).insert(item, (key, src.clone()));
                Some(src)
            }
            Err(e) => {
                // a missing proxy falls back to the full-resolution media
                log::warn!("proxy offline: {path}: {e}");
                None
            }
        }
    }

    /// A render-side provider bound to a project snapshot (proxies when enabled).
    pub fn provider(self: &Arc<Self>, project: Arc<Project>, services: Arc<dyn Services>) -> PoolProvider {
        PoolProvider { pool: self.clone(), project, services, full_res: false }
    }

    /// An empty pool with this one's openers, proxy mode and offline record (shared): its sources
    /// open the same files again with decoders of their own (fork 6E.3, the second GPU's lane of an
    /// export; the decoders are made on that lane's threads, so they decode on its GPU).
    pub fn lane_copy(&self) -> MediaPool {
        let openers = self.openers.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        MediaPool {
            sources: RwLock::new(HashMap::new()),
            proxies: RwLock::new(HashMap::new()),
            offline: self.offline.clone(),
            proxy_mode: AtomicU8::new(self.proxy_mode.load(std::sync::atomic::Ordering::Relaxed)),
            motion: AtomicBool::new(false),
            openers: RwLock::new(openers),
            handoff: Vec::new(),
            net_copies: self.net_copies.clone(),
        }
    }

    /// [`MediaPool::lane_copy`] whose sources hand their pictures off scaled by `scales` (per
    /// resolved media item; fork U0.2.3b d, `filmcraft_render::handoff::handoff_scales`): only
    /// for an export, whose requests never ask more of a source than that.
    pub fn lane_copy_scaled(&self, scales: &[(ItemId, f32)]) -> MediaPool {
        MediaPool { handoff: scales.iter().take(4096).copied().filter(|s| s.1.is_finite() && s.1 > 0.0 && s.1 < 1.0).collect(), ..self.lane_copy() }
    }

    /// A provider that always reads full-resolution media (export).
    pub fn full_res_provider(self: &Arc<Self>, project: Arc<Project>, services: Arc<dyn Services>) -> PoolProvider {
        PoolProvider { pool: self.clone(), project, services, full_res: true }
    }
}

/// Implements [`filmcraft_render::SourceProvider`] over the pool for one project snapshot.
pub struct PoolProvider {
    pub pool: Arc<MediaPool>,
    pub project: Arc<Project>,
    pub services: Arc<dyn Services>,
    /// Ignore proxies (export always renders full resolution).
    pub full_res: bool,
}

impl filmcraft_render::SourceProvider for PoolProvider {
    fn source(&self, item: ItemId) -> Option<SharedSource> {
        if self.full_res { self.pool.full_res_source(&self.project, item, &*self.services) } else { self.pool.source_for(&self.project, item, &*self.services) }
    }

    /// The same project snapshot over a [`MediaPool::lane_copy`] (fork 6E.3).
    fn lane_copy(&self) -> Option<Arc<dyn filmcraft_render::SourceProvider + Send + Sync>> {
        Some(Arc::new(PoolProvider {
            pool: Arc::new(self.pool.lane_copy()),
            project: self.project.clone(),
            services: self.services.clone(),
            full_res: self.full_res,
        }))
    }

    /// The same project snapshot over a [`MediaPool::lane_copy_scaled`] (fork U0.2.3b d).
    fn lane_copy_scaled(&self, scales: &[(ItemId, f32)]) -> Option<Arc<dyn filmcraft_render::SourceProvider + Send + Sync>> {
        Some(Arc::new(PoolProvider {
            pool: Arc::new(self.pool.lane_copy_scaled(scales)),
            project: self.project.clone(),
            services: self.services.clone(),
            full_res: self.full_res,
        }))
    }
}

/// The offline slate as a media source: video frames are the slate at the requested scale of the
/// item's frame size; audio is silence.
pub struct SlateSource {
    info: MediaInfo,
    name: String,
    reason: OfflineReason,
    last: Mutex<Option<Arc<VideoFrame>>>,
}

impl SlateSource {
    pub fn new(info: MediaInfo, name: &str, reason: OfflineReason) -> Self {
        Self { info, name: name.to_string(), reason, last: Mutex::new(None) }
    }
    pub fn reason(&self) -> OfflineReason {
        self.reason
    }
}

impl MediaSource for SlateSource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let v = self.info.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        let s = req.scale.clamp(1.0 / 64.0, 1.0);
        let (w, h) = (((v.width as f32 * s).round() as u32).max(2), ((v.height as f32 * s).round() as u32).max(2));
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(f) = last.as_ref()
            && f.width == w
            && f.height == h
        {
            return Ok(Arc::new((**f).clone().with_pts(req.time)));
        }
        let px = filmcraft_render::offline::slate_rgba8(w as usize, h as usize, &self.name, self.reason);
        let f = Arc::new(VideoFrame::rgba8(w, h, px));
        *last = Some(f.clone());
        Ok(Arc::new((*f).clone().with_pts(req.time)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, self.info.audio().map_or(2, |a| a.channels as usize), frames))
    }
}

/// A file's source whose picture shows the unreadable slate for each frame its decoder reports as
/// unsupported (H.264 High 4:2:2, a codec without a decoder...), instead of no picture at all,
/// and marks the item unreadable with the reason, so the monitors and the Project panel say why
/// (#626). Its sound plays as before.
struct UnsupportedAsSlate {
    inner: SharedSource,
    slate: SlateSource,
    item: ItemId,
    path: String,
    offline: Arc<RwLock<HashMap<ItemId, OfflineStatus>>>,
}

impl MediaSource for UnsupportedAsSlate {
    fn info(&self) -> &MediaInfo {
        self.inner.info()
    }
    fn decoder_name(&self) -> Option<String> {
        self.inner.decoder_name()
    }
    fn frame_clock(&self) -> filmcraft_media::timing::FrameClock {
        self.inner.frame_clock()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        match self.inner.video_frame(req) {
            Err(MediaError::Unsupported(why)) => {
                self.offline.write().unwrap_or_else(|e| e.into_inner()).entry(self.item).or_insert_with(|| {
                    log::warn!("media unreadable: {}: {why}", self.path);
                    OfflineStatus { reason: OfflineReason::Unreadable, path: self.path.clone(), error: why }
                });
                self.slate.video_frame(req)
            }
            r => r,
        }
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.inner.audio_stream(stream, start, frames, sample_rate)
    }
}

/// A proxy standing in for its full-resolution media: reports the full-resolution info and hands
/// out the proxy's (smaller) frames. A proxy whose aspect ratio differs from the original is
/// resampled to the original's aspect so it lines up exactly.
pub struct ProxySource {
    pub proxy: SharedSource,
    pub info: MediaInfo,
    /// Where audio comes from when the proxy has fewer audio streams than the original.
    pub audio_from: Option<SharedSource>,
}

impl MediaSource for ProxySource {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    /// The proxy's own frames are the ones shown (a proxy of a variable-frame-rate clip is
    /// constant-rate), so its clock, not the original's.
    fn frame_clock(&self) -> filmcraft_media::timing::FrameClock {
        self.proxy.frame_clock()
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let full = self.info.video.as_ref().ok_or(MediaError::NoStream("video"))?;
        let pv = self.proxy.info().video.as_ref().map(|v| (v.width, v.height)).unwrap_or((full.width, full.height));
        // ask the proxy for the scale relative to its own size
        let k = (full.width as f32 / pv.0.max(1) as f32).max(1.0);
        let f = self.proxy.video_frame(FrameRequest { time: req.time, scale: (req.scale * k).min(1.0) })?;
        let want_h = (f.width as f64 * full.height as f64 / full.width.max(1) as f64).round() as u32;
        if want_h.abs_diff(f.height) <= 1 {
            return Ok(f);
        }
        // different aspect: resample (nearest) to the original's aspect at the proxy's width
        let src = f.to_rgba8();
        let (w, h) = (f.width as usize, want_h.max(1) as usize);
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let sy = ((y as f64 + 0.5) * f.height as f64 / h as f64) as usize;
            let row = &src[sy.min(f.height as usize - 1) * w * 4..][..w * 4];
            out[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
        }
        Ok(Arc::new(VideoFrame::rgba8(w as u32, h as u32, out).with_pts(f.pts)))
    }
    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.audio_from.as_ref().unwrap_or(&self.proxy).audio(start, frames, sample_rate)
    }
    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        self.audio_from.as_ref().unwrap_or(&self.proxy).audio_stream(stream, start, frames, sample_rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::ItemKind;

    /// A host whose whole-file read fails: opening must go through the reader.
    struct ReaderOnly(Arc<[u8]>);
    impl Services for ReaderOnly {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            Err(std::io::Error::other(format!("{path}: read whole")))
        }
        fn write_file(&self, _path: &str, _data: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn reader(&self, _path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
            Some(Ok(Arc::new(filmcraft_media::reader::MemReader(self.0.clone()))))
        }
    }

    #[test]
    fn cyclic_subclips_do_not_recurse_in_media_or_rendering() {
        let mut project = Project::new("damaged references");
        let a = project.add_item(
            "A",
            filmcraft_project::Label::Iris,
            ItemKind::Subclip {
                parent: ItemId(999),
                range: filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::Tick(1000)),
                restrict_trims: false,
            },
            None,
        );
        let b = project.add_item(
            "B",
            filmcraft_project::Label::Iris,
            ItemKind::Subclip {
                parent: a,
                range: filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, filmcraft_time::Tick(1000)),
                restrict_trims: false,
            },
            None,
        );
        let pool = MediaPool::default();
        for parent in [a, b] {
            let ItemKind::Subclip { parent: reference, .. } = &mut project.item_mut(a).unwrap().kind else { panic!() };
            *reference = parent;
            assert!(pool.source_for(&project, a, &crate::FsServices).is_none());
            assert!(pool.full_res_source(&project, a, &crate::FsServices).is_none());
            assert!(crate::media_duration(&project, &pool, a).is_none());
            assert!(filmcraft_render::source_size(&project, a).is_none());
            assert!(project.resolve_media(a).is_none());
            assert!(filmcraft_render::colorman::override_of(&project, a).is_none());
            assert!(filmcraft_render::colorman::source_peak_nits(&project, a).is_none());
            assert!(crate::sync::project_clip(&project, a).is_none());
            let mut session = crate::Session { project: Arc::new(project.clone()), ..Default::default() };
            session.state.project_selection = vec![a];
            assert!(!session.is_enabled("file.mediaProperties"));
            assert!(session.execute("transcript.generate", serde_json::json!({"items":[a.0]})).is_err());
        }
    }

    /// Fork U0.2.3b d: a scaled lane copy keeps only sane scales below 1; plain copies and the
    /// pool itself hand off at full size.
    #[test]
    fn scaled_lane_copies_keep_sane_scales_only() {
        let pool = MediaPool::default();
        let (a, b) = (ItemId(1), ItemId(2));
        let copy = pool.lane_copy_scaled(&[(a, 0.5), (b, 1.0), (ItemId(3), f32::NAN), (ItemId(4), -1.0), (ItemId(5), 0.0)]);
        assert_eq!(copy.handoff, vec![(a, 0.5)]);
        assert!(pool.handoff.is_empty() && pool.lane_copy().handoff.is_empty() && copy.lane_copy().handoff.is_empty());
        let many: Vec<(ItemId, f32)> = (0..10_000).map(|i| (ItemId(i), 0.5)).collect();
        assert_eq!(pool.lane_copy_scaled(&many).handoff.len(), 4096, "capped");
    }

    /// Fork 6E.3: a lane copy is a separate pool (its own sources and decoders) with the same
    /// openers, proxy mode and offline record; the provider's copy reads the same snapshot.
    #[test]
    fn lane_copies_open_their_own_sources() {
        use filmcraft_render::SourceProvider;
        let pool = Arc::new(MediaPool::default());
        pool.set_proxy_mode(ProxyMode::On);
        let copy = pool.lane_copy();
        assert_eq!(copy.proxy_mode(), ProxyMode::On);
        assert_eq!(copy.openers.read().map(|o| o.len()).ok(), pool.openers.read().map(|o| o.len()).ok());
        assert!(Arc::ptr_eq(&copy.offline, &pool.offline));
        let wav = filmcraft_media::wav::write_wav16(&[0.0, 0.5, -0.5, 0.25], 2, 48_000);
        let a = copy.open_file("a.wav", &ReaderOnly(wav.clone().into())).expect("copy opens");
        let b = pool.open_file("a.wav", &ReaderOnly(wav.into())).expect("pool opens");
        assert!(!Arc::ptr_eq(&a, &b));
        let provider = pool.full_res_provider(Arc::new(Project::default()), Arc::new(crate::FsServices));
        let lane = provider.lane_copy().expect("a lane copy");
        assert!(lane.source(ItemId(1)).is_none(), "same (empty) snapshot");
        // a plain closure provider cannot make one
        let f = |_: ItemId| -> Option<SharedSource> { None };
        assert!(f.lane_copy().is_none());
    }

    #[test]
    fn open_file_prefers_the_hosts_reader() {
        let wav = filmcraft_media::wav::write_wav16(&[0.0, 0.5, -0.5, 0.25], 2, 48_000);
        let s = MediaPool::default().open_file("a.wav", &ReaderOnly(wav.into())).unwrap();
        assert!(s.info().has_audio());
    }

    /// The desktop host reads media in place instead of loading every clip into memory.
    #[cfg(any(unix, windows))]
    #[test]
    fn fs_services_open_media_through_a_file_reader() {
        let dir = std::env::temp_dir().join(format!("filmcraft-pool-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        let wav = filmcraft_media::wav::write_wav16(&[0.0, 0.5, -0.5, 0.25], 2, 48_000);
        std::fs::write(&path, &wav).unwrap();
        let p = path.to_string_lossy().to_string();
        let r = crate::FsServices.reader(&p).expect("a reader").unwrap();
        assert_eq!(r.len(), wav.len() as u64);
        assert!(MediaPool::default().open_file(&p, &crate::FsServices).unwrap().info().has_audio());
        let missing = dir.join("missing.wav").to_string_lossy().to_string();
        assert!(matches!(MediaPool::default().open_file(&missing, &crate::FsServices), Err(MediaError::Offline(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file whose first frame decodes and whose later frames need a decoder FilmCraft lacks.
    struct PartlyDecodable(MediaInfo);
    impl MediaSource for PartlyDecodable {
        fn info(&self) -> &MediaInfo {
            &self.0
        }
        fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            match req.time.0 {
                0 => Ok(Arc::new(VideoFrame::rgba8(2, 2, vec![7; 16]))),
                1 => Err(MediaError::Decode("damaged slice".into())),
                _ => Err(MediaError::Unsupported("H.264: field pictures".into())),
            }
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
            Ok(AudioBuffer::silence(sample_rate, 3, frames))
        }
    }

    /// #626: frames the decoder does not support show the unreadable slate (not nothing) and the
    /// item says why; decodable frames, damaged ones and the sound are left alone.
    #[test]
    fn undecodable_frames_show_the_unreadable_slate() {
        let info = GeneratorSource::new(filmcraft_media::Generator::ColorMatte { color: [0.0; 4] }, 16, 8, Default::default(), filmcraft_time::Tick(100))
            .info()
            .clone();
        let pool = MediaPool::default();
        let item = ItemId(1);
        pool.insert_file(item, "/media/cam/a.mov", Arc::new(PartlyDecodable(info)));
        let src = pool.cached(item).unwrap();
        let at = |t| src.video_frame(FrameRequest { time: filmcraft_time::Tick(t), scale: 1.0 });
        assert_eq!(at(0).unwrap().to_rgba8(), vec![7; 16]);
        assert!(matches!(at(1), Err(MediaError::Decode(_))));
        assert!(pool.offline_status(item).is_none(), "nothing unsupported yet");
        let f = at(2).unwrap();
        assert_eq!((f.width, f.height), (16, 8));
        assert_eq!(f.to_rgba8(), filmcraft_render::offline::slate_rgba8(16, 8, "a.mov", OfflineReason::Unreadable));
        let st = pool.offline_status(item).unwrap();
        assert_eq!((st.reason, st.path.as_str(), st.error.as_str()), (OfflineReason::Unreadable, "/media/cam/a.mov", "H.264: field pictures"));
        assert_eq!(src.audio(0, 4, 48_000).unwrap().channels.len(), 3);
        // linking the item to another file forgets the reason
        pool.insert_file(item, "/media/cam/b.mov", src.clone());
        assert!(pool.offline_status(item).is_none());
    }

    /// #626 end to end: an H.264 profile FilmCraft does not decode yet (4:4:4) imports and shows
    /// the unreadable slate with the decoder's reason, instead of a black picture.
    #[test]
    fn ffmpeg_unsupported_h264_profile_shows_the_unreadable_slate() {
        let Some(ffmpeg) = filmcraft_testkit::oracle::ffmpeg_or_skip("media_pool") else { return };
        let path = filmcraft_testkit::fixtures_dir("engine/media_pool").join("h264_444_64x32.mp4");
        let Some(path) = filmcraft_testkit::fixtures::generate(&path, |tmp| {
            std::process::Command::new(&ffmpeg)
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=64x32:rate=24:duration=0.5"])
                .args(["-c:v", "libx264", "-preset", "fast", "-pix_fmt", "yuv444p"])
                .arg(tmp)
                .status()
                .is_ok_and(|s| s.success())
        }) else {
            return;
        };
        let mut s = crate::Session::default();
        let r = s.execute("file.import", serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let item = ItemId(r["items"][0].as_u64().unwrap());
        let src = s.media.source_for(&s.project, item, &*s.services).unwrap();
        let f = src.video_frame(FrameRequest { time: filmcraft_time::Tick::ZERO, scale: 1.0 }).unwrap();
        assert_eq!(f.to_rgba8(), filmcraft_render::offline::slate_rgba8(64, 32, "h264_444_64x32.mp4", OfflineReason::Unreadable));
        let st = s.media.offline_status(item).unwrap();
        assert_eq!(st.reason, OfflineReason::Unreadable);
        assert!(st.error.contains("H.264") && st.error.contains("chroma_format_idc 3"), "{}", st.error);
    }
}
