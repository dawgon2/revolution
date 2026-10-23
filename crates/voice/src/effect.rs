//! The `voice_isolate` audio effect backed by DeepFilterNet3. [`install`] loads the checkpoint
//! once and registers the effect with `filmcraft-audio-dsp`
//! ([`filmcraft_audio_dsp::register_external`]), so every `create_effect("voice_isolate", …)`
//! (clip effects, mixer inserts, export) builds this one; without the model, or at sample rates
//! other than 48 kHz, the built-in DeNoise fallback is used instead.
//!
//! Each channel runs its own [`Denoiser`]: blocks of any length are queued into 10 ms hops, so
//! the effect's latency is one hop of queueing plus the model's 30 ms, 1920 samples in all.
//! Amount mixes the enhanced signal with the input delayed by the same 1920 samples.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use filmcraft_audio_dsp::{AudioEffect, ParamSpec};

use crate::ModelError;
use crate::denoise::Denoiser;
use crate::weights::Weights;

const ID: &str = "voice_isolate";
const HOP: usize = 480;
/// Hop queueing + STFT overlap + model lookahead.
pub const LATENCY: usize = HOP + 1440;
/// `limit`: the most the model may take away, in dB (fork AUDIO-PLAN A3.6, DeepFilterNet's
/// attenuation limit): the input is kept at least 10^(−limit / 20) underneath, so a mild noise
/// floor is lowered rather than removed and clean speech is barely touched. 100 dB = no limit.
static PARAMS: &[ParamSpec] = &[
    ParamSpec::new("amount", "Amount", 0.0, 100.0, 100.0, filmcraft_audio_dsp::Unit::Percent),
    ParamSpec::new("limit", "Attenuation Limit", 6.0, 100.0, 100.0, filmcraft_audio_dsp::Unit::Decibels),
];

/// The wet share for `amount` (%) under attenuation limit `limit` (dB); exactly `amount / 100`
/// without a limit (100 dB).
fn wet_share(amount: f32, limit: f32) -> f32 {
    let a = amount / 100.0;
    if limit >= 100.0 { a } else { a * (1.0 - 10f32.powf(-limit / 20.0)) }
}

/// The installed model: where it is, the loaded weights, and why loading failed (if it did).
#[derive(Default)]
struct Model {
    path: Option<PathBuf>,
    weights: Option<Arc<Weights>>,
    error: Option<String>,
}

fn model() -> &'static RwLock<Model> {
    static M: OnceLock<RwLock<Model>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// Load a DeepFilterNet3 checkpoint now and make `voice_isolate` use it.
pub fn install(path: &Path) -> Result<(), ModelError> {
    let w = Weights::load(path)?;
    install_weights(w)?;
    model().write().unwrap_or_else(PoisonError::into_inner).path = Some(path.to_path_buf());
    Ok(())
}

/// [`install`] with weights already loaded (tests, callers that loaded them elsewhere).
pub fn install_weights(w: Weights) -> Result<(), ModelError> {
    // Building one denoiser checks everything an effect instance will need.
    Denoiser::new(&w)?;
    {
        let mut m = model().write().unwrap_or_else(PoisonError::into_inner);
        m.weights = Some(Arc::new(w));
        m.error = None;
    }
    if !filmcraft_audio_dsp::register_external(ID, factory) {
        return Err(ModelError("this build has no `voice_isolate` effect".into()));
    }
    Ok(())
}

/// Use the checkpoint at `path` from now on, loading it the first time an effect needs it (so
/// starting the app costs nothing). A file that doesn't load leaves the DeNoise fallback in place
/// and is reported by [`status`].
pub fn set_model_path(path: &Path) -> bool {
    {
        let mut m = model().write().unwrap_or_else(PoisonError::into_inner);
        if m.path.as_deref() == Some(path) && (m.weights.is_some() || m.error.is_some()) {
            return true;
        }
        *m = Model { path: Some(path.to_path_buf()), weights: None, error: None };
    }
    filmcraft_audio_dsp::register_external(ID, factory)
}

/// Forget the model: `voice_isolate` falls back to DeNoise again.
pub fn uninstall() {
    filmcraft_audio_dsp::unregister_external(ID);
    *model().write().unwrap_or_else(PoisonError::into_inner) = Model::default();
}

/// Whether the model is loaded.
pub fn installed() -> bool {
    model().read().unwrap_or_else(PoisonError::into_inner).weights.is_some()
}

/// The model's state: (checkpoint path, loaded, load error).
pub fn status() -> (Option<PathBuf>, bool, Option<String>) {
    let m = model().read().unwrap_or_else(PoisonError::into_inner);
    (m.path.clone(), m.weights.is_some(), m.error.clone())
}

/// The weights, loading them from the set path on first use (once: a failure is remembered).
fn weights() -> Option<Arc<Weights>> {
    {
        let m = model().read().unwrap_or_else(PoisonError::into_inner);
        if let Some(w) = &m.weights {
            return Some(w.clone());
        }
        if m.path.is_none() || m.error.is_some() {
            return None;
        }
    }
    let mut m = model().write().unwrap_or_else(PoisonError::into_inner);
    if m.weights.is_none() && m.error.is_none() {
        let path = m.path.clone()?;
        match Weights::load(&path).and_then(|w| Denoiser::new(&w).map(|_| w)) {
            Ok(w) => m.weights = Some(Arc::new(w)),
            Err(e) => m.error = Some(e.0),
        }
    }
    m.weights.clone()
}

/// Run DeepFilterNet3 over whole 48 kHz channels (1 … 8 of equal length) offline, latency removed:
/// the "Faster" model of a rendered voice isolation (fork AUDIO-PLAN A3.7), the same processing as
/// the live effect at Amount 100 %, no limit. `progress(fraction)` returns `false` to stop. `Err`
/// when the model isn't installed / doesn't load, or it was stopped.
pub fn isolate_offline(channels: &[Vec<f32>], progress: &mut dyn FnMut(f32) -> bool) -> Result<Vec<Vec<f32>>, ModelError> {
    let w = weights().ok_or_else(|| ModelError(status().2.unwrap_or_else(|| "the DeepFilterNet3 voice model isn't installed".into())))?;
    let n = channels.first().map_or(0, Vec::len);
    if channels.is_empty() || channels.len() > 8 || channels.iter().any(|c| c.len() != n) {
        return Err(ModelError("1 to 8 channels of equal length are supported".into()));
    }
    let mut fx = VoiceIsolate::new(&w, channels.len())?;
    let mut bufs: Vec<Vec<f32>> = Vec::with_capacity(channels.len());
    for c in channels {
        let mut b = Vec::new();
        b.try_reserve_exact(n + LATENCY).map_err(|_| ModelError("not enough memory for this much audio".into()))?;
        b.extend_from_slice(c);
        b.resize(n + LATENCY, 0.0);
        bufs.push(b);
    }
    let total = n + LATENCY;
    let mut i = 0;
    while i < total {
        let end = (i + 48_000).min(total);
        let mut views: Vec<&mut [f32]> = bufs.iter_mut().filter_map(|b| b.get_mut(i..end)).collect();
        fx.process(&mut views);
        i = end;
        if !progress(i as f32 / total.max(1) as f32) {
            return Err(ModelError("cancelled".into()));
        }
    }
    for b in bufs.iter_mut() {
        b.drain(..LATENCY.min(b.len()));
        b.truncate(n);
    }
    Ok(bufs)
}

fn factory(sample_rate: f32, channels: usize) -> Option<Box<dyn AudioEffect>> {
    if (sample_rate - 48_000.0).abs() > 0.5 {
        return None;
    }
    let w = weights()?;
    VoiceIsolate::new(&w, channels).ok().map(|v| Box::new(v) as Box<dyn AudioEffect>)
}

/// One channel's queues around the shared denoiser (whose input buffers hold the hop so far).
struct Chan {
    outq: VecDeque<f32>,
    dry: VecDeque<f32>,
}

impl Chan {
    fn reset(&mut self) {
        self.outq.clear();
        self.outq.extend(std::iter::repeat_n(0.0, HOP));
        self.dry.clear();
        self.dry.extend(std::iter::repeat_n(0.0, LATENCY));
    }
}

/// DeepFilterNet3 voice isolation at 48 kHz: one denoiser for all channels (the network listens
/// to their mean and its mask is applied to each), so stereo costs about what mono does.
pub struct VoiceIsolate {
    amount: f32,
    /// Attenuation limit, dB (100 = none).
    limit: f32,
    /// Smoothed 0…1 mix.
    mix: f32,
    den: Denoiser,
    chans: Vec<Chan>,
    /// Samples of the current hop queued so far (the same in every channel).
    fill: usize,
    /// The model returned an error: pass the (delayed) input through from now on.
    failed: bool,
}

impl VoiceIsolate {
    pub fn new(w: &Weights, channels: usize) -> Result<VoiceIsolate, ModelError> {
        let den = Denoiser::with_channels(w, channels)?;
        let chans = (0..den.channels())
            .map(|_| {
                let mut c = Chan { outq: VecDeque::with_capacity(2 * HOP), dry: VecDeque::with_capacity(LATENCY + 1) };
                c.reset();
                c
            })
            .collect();
        Ok(VoiceIsolate { amount: 100.0, limit: 100.0, mix: 1.0, den, chans, fill: 0, failed: false })
    }
}

impl AudioEffect for VoiceIsolate {
    fn id(&self) -> &'static str {
        ID
    }
    fn params(&self) -> &'static [ParamSpec] {
        PARAMS
    }
    fn set_param(&mut self, id: &str, value: f32) -> bool {
        let Some(spec) = PARAMS.iter().find(|p| p.id == id) else { return false };
        let v = spec.sanitize(value);
        match id {
            "amount" => self.amount = v,
            _ => self.limit = v,
        }
        true
    }
    fn param(&self, id: &str) -> Option<f32> {
        match id {
            "amount" => Some(self.amount),
            "limit" => Some(self.limit),
            _ => None,
        }
    }
    fn reset(&mut self) {
        self.mix = wet_share(self.amount, self.limit);
        self.den.reset();
        self.fill = 0;
        self.failed = false;
        self.chans.iter_mut().for_each(Chan::reset);
    }
    fn latency(&self) -> usize {
        LATENCY
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        // (channels beyond the ones built for pass through untouched)
        let n = channels.len().min(self.chans.len());
        let target = wet_share(self.amount, self.limit);
        let mut i = 0;
        while i < len {
            // up to the end of the current hop
            let take = (len - i).min(HOP - self.fill);
            let mix0 = self.mix;
            for (c, (buf, ch)) in channels.iter_mut().zip(self.chans.iter_mut()).take(n).enumerate() {
                let mut mix = mix0;
                let fill = self.fill;
                let (Some(src), Some(q)) = (buf.get_mut(i..i + take), self.den.input_mut(c).and_then(|q| q.get_mut(fill..fill + take))) else { break };
                for (v, slot) in src.iter_mut().zip(q.iter_mut()) {
                    let x = if v.is_finite() { *v } else { 0.0 };
                    *slot = x;
                    ch.dry.push_back(x);
                    let dry = ch.dry.pop_front().unwrap_or(0.0);
                    let wet = ch.outq.pop_front().unwrap_or(0.0);
                    mix += (target - mix) * 0.002;
                    *v = if self.failed { dry } else { mix * wet + (1.0 - mix) * dry };
                }
            }
            for _ in 0..take {
                self.mix += (target - self.mix) * 0.002;
            }
            self.fill += take;
            if self.fill >= HOP {
                self.fill = 0;
                if !self.failed && self.den.step(n).is_err() {
                    self.failed = true;
                }
                for (c, ch) in self.chans.iter_mut().take(n).enumerate() {
                    match self.den.output(c) {
                        Some(out) if !self.failed => ch.outq.extend(out.iter().copied()),
                        _ => ch.outq.extend(std::iter::repeat_n(0.0, HOP)),
                    }
                }
            }
            i += take;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{Device, Tensor};

    fn synthetic() -> Weights {
        let widths = crate::dsp::erb_widths(48_000, 960, 32, 2).unwrap();
        let mut fb = vec![0.0f32; 481 * 32];
        let mut start = 0;
        for (b, w) in widths.iter().enumerate() {
            for k in start..start + w {
                fb[k * 32 + b] = 1.0 / *w as f32;
            }
            start += w;
        }
        let v = crate::weights::manifest()
            .into_iter()
            .map(|(n, d)| {
                let t = if n == "erb_fb" {
                    Tensor::from_vec(fb.clone(), d.as_slice(), &Device::Cpu).unwrap()
                } else {
                    Tensor::full(0.01f32, d.as_slice(), &Device::Cpu).unwrap()
                };
                (n, t)
            })
            .collect();
        Weights::from_tensors(v).unwrap()
    }

    fn signal(n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 0.031).sin() * 0.3 + ((i * 7919) % 211) as f32 / 2000.0).collect()
    }

    /// One test owns the global registration (tests in this crate run in parallel).
    #[test]
    fn install_registers_and_uninstall_falls_back() {
        let fallback_latency = filmcraft_audio_dsp::create_effect(ID, 48_000.0, 2).unwrap().latency();
        assert_ne!(fallback_latency, LATENCY);
        install_weights(synthetic()).unwrap();
        assert!(installed());
        let mut fx = filmcraft_audio_dsp::create_effect(ID, 48_000.0, 2).unwrap();
        assert_eq!((fx.id(), fx.latency()), (ID, LATENCY));
        // Other sample rates keep the fallback.
        assert_eq!(
            filmcraft_audio_dsp::create_effect(ID, 44_100.0, 2).unwrap().latency(),
            filmcraft_audio_dsp::create_effect("denoise", 44_100.0, 2).unwrap().latency()
        );

        // Amount 0 is the input delayed by the latency, for any block sizes.
        assert!(fx.set_param("amount", 0.0));
        fx.reset();
        let x = signal(9000);
        let (mut l, mut r) = (x.clone(), x.clone());
        let mut done = 0;
        for size in [1, 333, 1000, 480, 2048, 7].iter().cycle() {
            if done >= x.len() {
                break;
            }
            let end = (done + size).min(x.len());
            let (a, b) = (&mut l[done..end], &mut r[done..end]);
            fx.process(&mut [a, b]);
            done = end;
        }
        for i in LATENCY..x.len() {
            assert_eq!(l[i], x[i - LATENCY], "sample {i}");
        }
        assert_eq!(l, r);

        // Hostile input (non-finite samples count as silence): finite output, no panic; extra channels pass through untouched.
        fx.set_param("amount", 100.0);
        let mut a = vec![f32::NAN; 1000];
        let mut b = vec![1e30f32; 1000];
        let mut c = vec![0.5f32; 1000];
        fx.process(&mut [&mut a, &mut b, &mut c]);
        assert!(a.iter().chain(&b).all(|v| v.is_finite()));
        assert!(c.iter().all(|v| *v == 0.5));
        assert!(!fx.set_param("reduction", 1.0));
        assert_eq!(fx.param("amount"), Some(100.0));

        uninstall();
        assert!(!installed());
        assert_eq!(filmcraft_audio_dsp::create_effect(ID, 48_000.0, 2).unwrap().latency(), fallback_latency);

        // A lazily set path that doesn't load: the fallback stays, the error is reported once.
        let bad = std::env::temp_dir().join(format!("fc-voice-bad-{}.ckpt", std::process::id()));
        std::fs::write(&bad, b"not a checkpoint").unwrap();
        assert!(set_model_path(&bad));
        assert_eq!(status(), (Some(bad.clone()), false, None));
        assert_eq!(filmcraft_audio_dsp::create_effect(ID, 48_000.0, 2).unwrap().latency(), fallback_latency);
        let (_, loaded, error) = status();
        assert!(!loaded && error.is_some());
        uninstall();
        std::fs::remove_file(&bad).ok();
        assert_eq!(status(), (None, false, None));
    }

    #[test]
    fn attenuation_limit_keeps_the_input_underneath() {
        // no limit: exactly the old wet mix
        for a in [0.0f32, 37.5, 80.0, 100.0] {
            assert_eq!(wet_share(a, 100.0), a / 100.0);
        }
        // a 20 dB limit keeps a tenth of the input
        assert!((wet_share(100.0, 20.0) - 0.9).abs() < 1e-6);
        assert!((wet_share(50.0, 20.0) - 0.45).abs() < 1e-6);
        let mut fx = VoiceIsolate::new(&synthetic(), 1).unwrap();
        assert_eq!(fx.param("limit"), Some(100.0));
        assert!(fx.set_param("limit", 12.0));
        assert_eq!(fx.param("limit"), Some(12.0));
        // hostile values: clamped or the default, never NaN in the mix
        assert!(fx.set_param("limit", f32::NAN));
        assert_eq!(fx.param("limit"), Some(100.0));
        assert!(fx.set_param("limit", -1e9));
        assert_eq!(fx.param("limit"), Some(6.0));
        assert!(fx.set_param("limit", f32::INFINITY));
        assert_eq!(fx.param("limit"), Some(100.0));
        assert!(!fx.set_param("nope", 1.0));
        fx.reset();
        let mut buf = vec![0.25f32; 4096];
        fx.process(&mut [&mut buf]);
        assert!(buf.iter().all(|v| v.is_finite()));
    }
}
