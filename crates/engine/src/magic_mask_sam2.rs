//! Magic Mask's SAM 2.1 tracker in the engine (fork G3.4, FORK-PLAN §1a.124): Better = Hiera
//! Large, Faster = Hiera Base+, both from `<data dir>/models/sam2/` (the official checkpoint and
//! YAML config). The heavy parts run on the GPU ([`filmcraft_gpu::sam2::Sam2Gpu`]); the CPU
//! reference is far too slow for tracking, so without a usable GPU the track falls back to
//! MobileSAM (unchanged). `Auto` is Better on a discrete GPU (owner question Q66).
//!
//! One model is kept loaded (weights on the GPU, the decoder's on the CPU; loading the other
//! size replaces it), and released after [`IDLE_RELEASE`] when no track uses it; a track makes
//! its own [`Sam2Tracker`] (the memory of that clip). A model whose GPU failed is forgotten
//! ([`forget`]) so the next track loads it afresh.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use web_time::Instant;

use filmcraft_gpu::sam2::{GpuBlock, Sam2Gpu, Sam2GpuSpec, Weight};
use filmcraft_mask::MaskError;
use filmcraft_mask::sam2::heads::Sam2Heads;
use filmcraft_mask::sam2::tracker::Sam2Tracker;
use filmcraft_mask::sam2::weights::{HEAVY_PREFIXES, Sam2Weights};
use filmcraft_mask::sam2::{Sam2Compute, Sam2Config, Sam2Features, Sam2Size};

/// The GPU passes as the mask crate's [`Sam2Compute`].
struct GpuCompute(Sam2Gpu);

fn failed(e: impl std::fmt::Display) -> MaskError {
    MaskError::Failed(e.to_string())
}

impl Sam2Compute for GpuCompute {
    fn name(&self) -> String {
        self.0.name().to_string()
    }

    fn image(&self, input: &[f32]) -> Result<Sam2Features, MaskError> {
        #[cfg(test)]
        if FAIL_NEXT.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(MaskError::Failed("injected GPU failure".into()));
        }
        let (s0, s1, feat) = self.0.image(input).map_err(failed)?;
        Ok(Sam2Features { s0, s1, feat })
    }

    fn memory_attention(&self, feat: &[f32], memory: &[f32], memory_pos: &[f32], ptr_tokens: usize) -> Result<Vec<f32>, MaskError> {
        self.0.memory_attention(feat, memory, memory_pos, ptr_tokens).map_err(failed)
    }

    fn memory_encoder(&self, feat: &[f32], mask: &[f32]) -> Result<Vec<f32>, MaskError> {
        self.0.memory_encoder(feat, mask).map_err(failed)
    }
}

/// A loaded SAM 2.1 (one size).
pub(crate) struct Sam2Model {
    compute: Arc<dyn Sam2Compute>,
    heads: Arc<Sam2Heads>,
    pub(crate) size: Sam2Size,
    pub(crate) discrete: bool,
}

impl Sam2Model {
    /// A tracker for a clip of `frames` frames.
    pub(crate) fn tracker(&self, frames: usize) -> Result<Sam2Tracker, MaskError> {
        Sam2Tracker::new(self.compute.clone(), self.heads.clone(), frames)
    }

    pub(crate) fn name(&self) -> String {
        format!("{} on {}", self.size.label(), self.compute.name())
    }
}

/// The checkpoint and config of `size` in `dir`, when both are there.
pub(crate) fn files_in(dir: &Path, size: Sam2Size) -> Option<(PathBuf, PathBuf)> {
    let (pt, yaml) = size.files();
    let (pt, yaml) = (dir.join(pt), dir.join(yaml));
    (pt.is_file() && yaml.is_file()).then_some((pt, yaml))
}

/// Where SAM 2.1 lives in the data folder.
pub(crate) fn default_dir() -> Option<PathBuf> {
    crate::transcript::models_dir().map(|d| d.join("sam2"))
}

/// The GPU's description of a checkpoint.
fn spec(w: &Sam2Weights) -> Result<Sam2GpuSpec, MaskError> {
    let cfg = &w.cfg;
    let c = w.constants()?;
    let e = cfg.embed_dim;
    Ok(Sam2GpuSpec {
        embed_dim: e,
        blocks: cfg.blocks().iter().map(|b| GpuBlock { dim: b.dim, dim_out: b.dim_out, heads: b.heads, window: b.window, pool: b.pool, res: b.res }).collect(),
        stage_ends: cfg.stage_ends(),
        stage_dims: [e, 2 * e, 4 * e, 8 * e],
        mem_layers: cfg.mem_layers,
        mem_ffn: cfg.mem_ffn,
        fuser_layers: cfg.fuser_layers,
        hiera_pos: c.hiera_pos,
        vision_pos: c.vision_pos,
        rope_cos: c.rope_cos,
        rope_sin: c.rope_sin,
    })
}

/// Tests: the next image encode fails as a lost GPU would.
#[cfg(test)]
pub(crate) static FAIL_NEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Unused this long (no track holding it), the loaded model is released.
pub(crate) const IDLE_RELEASE: Duration = Duration::from_secs(120);
#[cfg(not(target_arch = "wasm32"))]
const REAP_EVERY: Duration = Duration::from_secs(15);

/// The loaded model: its size, the checkpoint it came from, and when a track last took it.
struct Loaded {
    size: Sam2Size,
    path: PathBuf,
    model: Arc<Sam2Model>,
    used: Instant,
}

static LOADED: Mutex<Option<Loaded>> = Mutex::new(None);
#[cfg(not(target_arch = "wasm32"))]
static REAPER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Releases the model when it has been idle for `idle` and no track holds it.
pub(crate) fn release_if_idle(idle: Duration) {
    let mut g = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
    if g.as_ref().is_some_and(|l| l.used.elapsed() >= idle && Arc::strong_count(&l.model) == 1) {
        *g = None;
        log::info!("Magic Mask: SAM 2.1 released (idle)");
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn start_reaper() {
    use std::sync::atomic::Ordering;
    if REAPER.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("magic-mask-sam2-reaper".into()).spawn(|| {
        loop {
            std::thread::sleep(REAP_EVERY);
            release_if_idle(IDLE_RELEASE);
        }
    });
    if spawned.is_err() {
        REAPER.store(false, Ordering::SeqCst);
    }
}

/// Forget the loaded `size` (its GPU failed): the next track loads it again.
pub(crate) fn forget(size: Sam2Size) {
    let mut g = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
    if g.as_ref().is_some_and(|l| l.size == size) {
        *g = None;
    }
}

/// Whether `size` is loaded (tests).
#[cfg(test)]
pub(crate) fn is_loaded(size: Sam2Size) -> bool {
    LOADED.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(|l| l.size == size)
}
/// Held while a model loads (two first tracks don't load it twice).
static LOADING: Mutex<()> = Mutex::new(());

/// Load `size` from `files` (cached), its heavy parts on the GPU. No usable GPU (or
/// `FILMCRAFT_NO_GPU_MASK`) is an error: the caller falls back to MobileSAM.
pub(crate) fn load(size: Sam2Size, files: &(PathBuf, PathBuf)) -> Result<Arc<Sam2Model>, MaskError> {
    let _loading = LOADING.lock().unwrap_or_else(PoisonError::into_inner);
    {
        let mut g = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(l) = g.as_mut().filter(|l| l.size == size && l.path == files.0) {
            l.used = Instant::now();
            return Ok(l.model.clone());
        }
        // one model at a time: the other size's GPU memory goes (when its tracks end)
        *g = None;
    }
    if cfg!(target_arch = "wasm32") || std::env::var_os("FILMCRAFT_NO_GPU_MASK").is_some() {
        return Err(MaskError::Failed("SAM 2 needs the GPU, which is off for Magic Mask".into()));
    }
    let text = std::fs::read_to_string(&files.1).map_err(|_| MaskError::ModelMissing(files.1.display().to_string()))?;
    let cfg = Sam2Config::from_yaml(&text)?;
    let mut w = Sam2Weights::load(&files.0, cfg)?;
    let spec = spec(&w)?;
    let plain: HashMap<String, Weight> = w.plain_with(&HEAVY_PREFIXES)?.into_iter().map(|(k, t)| (k, Weight { shape: t.shape, data: t.data })).collect();
    // the plain copy goes to the GPU; the candle one isn't needed (the CPU fallback is MobileSAM)
    w.release(&HEAVY_PREFIXES);
    let opened = catch_unwind(AssertUnwindSafe(|| Sam2Gpu::open(&spec, &plain).map_err(failed)));
    drop(plain);
    let gpu = match opened {
        Ok(r) => r?,
        Err(_) => return Err(MaskError::Failed("the GPU stopped unexpectedly while loading SAM 2".into())),
    };
    let w = Arc::new(w);
    let discrete = gpu.discrete();
    let model = Arc::new(Sam2Model { compute: Arc::new(GpuCompute(gpu)), heads: Arc::new(Sam2Heads::new(w)?), size, discrete });
    log::info!("Magic Mask: {} loaded", model.name());
    *LOADED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Loaded { size, path: files.0.clone(), model: model.clone(), used: Instant::now() });
    #[cfg(not(target_arch = "wasm32"))]
    start_reaper();
    Ok(model)
}
