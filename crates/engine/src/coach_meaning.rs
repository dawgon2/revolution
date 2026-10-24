//! Search by meaning for the assistant (fork FORK-PLAN §1a.98, owner question Q45 (a); the model
//! download is owner question Q51).
//!
//! A small sentence-embedding model (BAAI/bge-small-en-v1.5, MIT, on FilmCraft's own forward pass
//! in `filmcraft_llm::embed`) turns every guide section the assistant's word index holds into a
//! vector once; a question is then matched by cosine similarity. [`coach_ask::retrieve_in`] uses it
//! alone ([`SearchMode::Meaning`]) or fused with the word ranking ([`SearchMode::Mixed`],
//! reciprocal rank fusion). Without the model (not downloaded, or a build without feature `llm`)
//! [`rank`] returns `None` and callers keep the word search.
//!
//! The vectors are cached next to the model (`guide-vectors.bin`), keyed by the model's revision
//! and every section's text, so a changed guide is embedded again and a damaged cache file is
//! ignored, never trusted.
//!
//! [`coach_ask::retrieve_in`]: crate::coach_ask::retrieve_in

/// Where the model lives under the models folder (`<data dir>/models`).
pub const MODEL_DIR: &str = "embed/bge-small-en-v1.5";

/// Which ranking [`crate::coach_ask::retrieve_in`] uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    /// BM25 over words with the coach's synonyms (the search before §1a.98).
    Words,
    /// Cosine similarity of the question's and the sections' embeddings.
    Meaning,
    /// Both rankings fused by reciprocal rank ([`RRF_K`]).
    Mixed,
}

/// What [`crate::coach_ask::retrieve`] uses. Words until the coverage measurement keeps meaning
/// (§1a.98: kept only if creative coverage rises without costing lookup hits; provisional, Q51).
pub const DEFAULT_MODE: SearchMode = SearchMode::Words;

/// [`DEFAULT_MODE`], or `FILMCRAFT_ASSIST_SEARCH` = `words` / `meaning` / `mixed` (for measuring the
/// real assistant with each, and for trying it).
pub fn default_mode() -> SearchMode {
    match std::env::var("FILMCRAFT_ASSIST_SEARCH").ok().as_deref().map(str::trim) {
        Some("words") => SearchMode::Words,
        Some("meaning") => SearchMode::Meaning,
        Some("mixed") => SearchMode::Mixed,
        _ => DEFAULT_MODE,
    }
}

/// Reciprocal rank fusion constant (the usual 60: ranks far down the list still count a little).
pub const RRF_K: f64 = 60.0;

/// How much the meaning ranking counts against the word ranking in [`SearchMode::Mixed`]
/// (1.0 = equal). `FILMCRAFT_MEANING_WEIGHT` overrides it for measuring (tests only read it).
pub const MEANING_WEIGHT: f64 = 1.0;

/// In [`SearchMode::Mixed`], the word ranking's first this many sections stay first (lookups keep
/// their hits); the fused ranking fills the rest. `FILMCRAFT_WORD_KEEP` overrides it for measuring.
pub const WORD_KEEP: usize = 3;

/// [`WORD_KEEP`], or the measuring override.
pub fn word_keep() -> usize {
    std::env::var("FILMCRAFT_WORD_KEEP").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(WORD_KEEP).min(crate::coach_ask::MAX_RETRIEVE)
}

/// In [`SearchMode::Mixed`], a word-ranked section among the first [`crate::coach_assist::SECTIONS`]
/// whose score is at least this share of the best word score also stays (a strong word match is
/// rarely wrong). `FILMCRAFT_WORD_KEEP_SHARE` overrides it for measuring.
pub const WORD_KEEP_SHARE: f64 = 0.45;

/// [`WORD_KEEP_SHARE`], or the measuring override.
pub fn word_keep_share() -> f64 {
    std::env::var("FILMCRAFT_WORD_KEEP_SHARE").ok().and_then(|v| v.parse::<f64>().ok()).filter(|w| w.is_finite()).unwrap_or(WORD_KEEP_SHARE)
}

/// [`MEANING_WEIGHT`], or the measuring override.
pub fn meaning_weight() -> f64 {
    std::env::var("FILMCRAFT_MEANING_WEIGHT").ok().and_then(|v| v.parse::<f64>().ok()).filter(|w| w.is_finite() && *w >= 0.0).unwrap_or(MEANING_WEIGHT)
}

/// Cosine at or below which a section counts as unrelated in meaning: a meaning-ranked section's
/// relevance share (for the pick prompt's filters) is its cosine's place between this and the best
/// cosine. Provisional (Q51): set from the model's measured cosines of off-topic questions.
pub const COS_FLOOR: f64 = 0.5;

/// BGE's instruction for short queries matched against passages (from the model card).
#[cfg(feature = "llm")]
const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";
/// Characters of a section sent to the model (it reads at most 512 tokens anyway).
#[cfg(feature = "llm")]
const MAX_SECTION_CHARS: usize = 2_400;
/// Characters of a question embedded.
#[cfg(feature = "llm")]
const MAX_QUERY_CHARS: usize = 1_000;
/// Most sections the cache file may hold, and the largest vector it may hold.
#[cfg(feature = "llm")]
const MAX_CACHED: usize = 100_000;
#[cfg(feature = "llm")]
const MAX_DIM: usize = 4_096;
#[cfg(feature = "llm")]
const CACHE_FILE: &str = "guide-vectors.bin";
#[cfg(feature = "llm")]
const MAGIC: &[u8; 8] = b"FCGVEC01";

#[cfg(feature = "llm")]
struct Meaning {
    embedder: filmcraft_llm::embed::Embedder,
    /// One unit vector per assistant section, in the word index's order.
    vecs: Vec<Vec<f32>>,
}

/// Whether search by meaning can run (the model is installed and loaded).
pub fn available() -> bool {
    #[cfg(feature = "llm")]
    {
        meaning().is_some()
    }
    #[cfg(not(feature = "llm"))]
    {
        false
    }
}

/// The `n` sections most similar in meaning to `question` as (section index in the assistant's
/// index, cosine), best first; `None` without the model.
pub fn rank(question: &str, n: usize) -> Option<Vec<(usize, f64)>> {
    #[cfg(feature = "llm")]
    {
        let m = meaning()?;
        let q: String = question.chars().take(MAX_QUERY_CHARS).collect();
        let v = m.embedder.embed(&format!("{QUERY_PREFIX}{q}")).ok()?;
        let mut ranked: Vec<(usize, f64)> = m.vecs.iter().enumerate().map(|(i, s)| (i, f64::from(dot(&v, s)))).filter(|(_, c)| c.is_finite()).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(n);
        // nothing comparable (a damaged model gave NaNs): fall back to the word search
        (!ranked.is_empty()).then_some(ranked)
    }
    #[cfg(not(feature = "llm"))]
    {
        let _ = (question, n);
        None
    }
}

#[cfg(feature = "llm")]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The loaded model: `None` until first asked, `Some(None)` when it isn't installed or failed.
#[cfg(feature = "llm")]
static MEANING: std::sync::Mutex<Option<Option<std::sync::Arc<Meaning>>>> = std::sync::Mutex::new(None);

#[cfg(feature = "llm")]
fn meaning() -> Option<std::sync::Arc<Meaning>> {
    // (held while loading: two first questions don't load it twice)
    let mut g = MEANING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    g.get_or_insert_with(|| {
        let dir = crate::transcript::models_dir()?.join(MODEL_DIR);
        if !dir.join("model.safetensors").is_file() {
            return None;
        }
        // the forward pass returns errors, but a model file is hostile input: never let a
        // surprise in it take the assistant down with it
        std::panic::catch_unwind(|| load(&dir)).ok().flatten().map(std::sync::Arc::new)
    })
    .clone()
}

/// Whether the model is in memory now (never loads it).
pub fn loaded() -> bool {
    #[cfg(feature = "llm")]
    {
        MEANING.try_lock().map_or(true, |g| g.as_ref().is_some_and(Option::is_some))
    }
    #[cfg(not(feature = "llm"))]
    {
        false
    }
}

/// Free the model (`ai.stop`); the next question loads it again. Never waits: false while a
/// search holds the lock.
pub fn release() -> bool {
    #[cfg(feature = "llm")]
    {
        match MEANING.try_lock() {
            Ok(mut g) => {
                *g = None;
                true
            }
            Err(std::sync::TryLockError::Poisoned(p)) => {
                *p.into_inner() = None;
                true
            }
            Err(std::sync::TryLockError::WouldBlock) => false,
        }
    }
    #[cfg(not(feature = "llm"))]
    {
        true
    }
}

#[cfg(feature = "llm")]
fn load(dir: &std::path::Path) -> Option<Meaning> {
    let embedder = filmcraft_llm::embed::Embedder::load(dir).ok()?;
    let texts = crate::coach_ask::assist_embed_texts(MAX_SECTION_CHARS);
    let revision = read_revision(dir);
    let key = cache_key(revision.trim(), embedder.dim(), &texts);
    let path = dir.join(CACHE_FILE);
    if let Some(vecs) = read_cache(&path, key, texts.len(), embedder.dim()) {
        return Some(Meaning { embedder, vecs });
    }
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let vecs = embedder.embed_batch(&refs).ok()?;
    if vecs.len() != texts.len() || vecs.iter().any(|v| v.len() != embedder.dim()) {
        return None;
    }
    // a cache that can't be written only costs the next start the embedding time
    let _ = write_cache(&path, key, &vecs);
    Some(Meaning { embedder, vecs })
}

/// The model's `REVISION` file, at most 256 bytes of it (a hostile file is never read whole).
#[cfg(feature = "llm")]
fn read_revision(dir: &std::path::Path) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(dir.join("REVISION")) {
        let _ = f.take(256).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// FNV-1a over the revision, the dimension and every text (with its length, so boundaries count).
#[cfg(feature = "llm")]
fn cache_key(revision: &str, dim: usize, texts: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(revision.as_bytes());
    eat(&(dim as u64).to_le_bytes());
    for t in texts {
        eat(&(t.len() as u64).to_le_bytes());
        eat(t.as_bytes());
    }
    h
}

/// The cached vectors when the file is exactly what `key`, `n` and `dim` say; anything else is
/// `None` (embedded again).
#[cfg(feature = "llm")]
fn read_cache(path: &std::path::Path, key: u64, n: usize, dim: usize) -> Option<Vec<Vec<f32>>> {
    if n > MAX_CACHED || dim == 0 || dim > MAX_DIM {
        return None;
    }
    let want = n.checked_mul(dim)?.checked_mul(4)?.checked_add(MAGIC.len() + 8 + 8)?;
    if std::fs::metadata(path).ok()?.len() != want as u64 {
        return None;
    }
    let raw = std::fs::read(path).ok()?;
    if raw.len() != want || raw.get(..MAGIC.len())? != MAGIC {
        return None;
    }
    let num = |at: usize| raw.get(at..at.checked_add(8)?).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes);
    let (k, shape) = (num(MAGIC.len())?, num(MAGIC.len() + 8)?);
    if k != key || shape != ((n as u64) << 32 | dim as u64) {
        return None;
    }
    let floats: Vec<f32> = raw.get(MAGIC.len() + 16..)?.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    if floats.iter().any(|x| !x.is_finite()) {
        return None;
    }
    Some(floats.chunks_exact(dim).map(<[f32]>::to_vec).collect())
}

/// Written to a `.part` file, then renamed over the cache.
#[cfg(feature = "llm")]
fn write_cache(path: &std::path::Path, key: u64, vecs: &[Vec<f32>]) -> std::io::Result<()> {
    let dim = vecs.first().map_or(0, Vec::len);
    let mut out = Vec::with_capacity(MAGIC.len() + 16 + vecs.len().saturating_mul(dim).saturating_mul(4));
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&key.to_le_bytes());
    out.extend_from_slice(&((vecs.len() as u64) << 32 | dim as u64).to_le_bytes());
    for v in vecs {
        for x in v {
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    let part = path.with_extension("bin.part");
    std::fs::write(&part, &out)?;
    std::fs::rename(&part, path)
}

#[cfg(all(test, feature = "llm"))]
mod tests {
    use super::*;

    #[test]
    fn the_cache_round_trips_and_refuses_anything_else() {
        let dir = std::env::temp_dir().join(format!("fc-gvec-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(CACHE_FILE);
        let vecs = vec![vec![0.5_f32, -0.25, 1.0], vec![0.0, 2.0, -3.5]];
        assert!(write_cache(&path, 42, &vecs).is_ok());
        assert_eq!(read_cache(&path, 42, 2, 3), Some(vecs.clone()));
        // another key, count or dimension: embedded again
        assert_eq!(read_cache(&path, 43, 2, 3), None);
        assert_eq!(read_cache(&path, 42, 3, 3), None);
        assert_eq!(read_cache(&path, 42, 1, 6), None);
        assert_eq!(read_cache(&path, 42, usize::MAX, usize::MAX), None);
        // a damaged file (truncated, a NaN, a wrong magic) is never trusted
        let good = std::fs::read(&path).unwrap_or_default();
        let _ = std::fs::write(&path, &good[..good.len() - 1]);
        assert_eq!(read_cache(&path, 42, 2, 3), None);
        let mut nan = good.clone();
        let at = nan.len() - 4;
        nan[at..].copy_from_slice(&f32::NAN.to_le_bytes());
        let _ = std::fs::write(&path, &nan);
        assert_eq!(read_cache(&path, 42, 2, 3), None);
        let mut magic = good;
        magic[0] = b'X';
        let _ = std::fs::write(&path, &magic);
        assert_eq!(read_cache(&path, 42, 2, 3), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cache_key_sees_every_text_and_its_boundaries() {
        let k = |r: &str, d: usize, t: &[&str]| cache_key(r, d, &t.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(k("a", 3, &["x", "y"]), k("a", 3, &["x", "y"]));
        assert_ne!(k("a", 3, &["x", "y"]), k("b", 3, &["x", "y"]));
        assert_ne!(k("a", 3, &["x", "y"]), k("a", 4, &["x", "y"]));
        assert_ne!(k("a", 3, &["xy", ""]), k("a", 3, &["x", "y"]));
    }
}
