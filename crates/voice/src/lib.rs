//! FilmCraft voice isolation (fork AUDIO-PLAN A2): a pure-Rust port of DeepFilterNet3
//! (Schröter et al., MIT / Apache-2.0). [`dsp`] is the model's signal path around the network:
//! the 960 / 480 Vorbis STFT ([`fft`] adds the mixed-radix FFT it needs), the ERB band layout,
//! the normalised ERB and complex input features, ERB gains and the deep-filter operation.
//! With feature `dfn`: `weights` reads and checks the released checkpoint, `net` is the network
//! (plain-Rust streaming kernels, one 10 ms frame at a time) and `denoise::Denoiser` enhances one
//! 48 kHz channel; `effect` is the `voice_isolate` audio effect (`effect::install` registers it with
//! `filmcraft-audio-dsp`). See `docs/fork/PREP-A2.md`.

#[cfg(feature = "dfn")]
pub mod denoise;
pub mod dsp;
#[cfg(feature = "dfn")]
pub mod effect;
pub mod fft;
pub mod gate;
pub mod kaldi;
#[cfg(feature = "dfn")]
pub mod mossformer;
#[cfg(feature = "dfn")]
pub mod net;
#[cfg(feature = "dfn")]
pub mod package;
pub mod testset;
#[cfg(feature = "dfn")]
pub mod weights;

pub use dsp::{C32, DfParams};

/// A model file that can't be used (missing, unreadable, not DeepFilterNet3, corrupt).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelError(pub String);

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelError {}
