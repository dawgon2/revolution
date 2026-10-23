//! The signal path around MossFormer2_SE_48K (fork AUDIO-PLAN A3.7), ported from ClearerVoice-Studio
//! (Alibaba / ModelScope, Apache-2.0: `clearvoice/utils/misc.py`, `utils/decode.py`, inference
//! config `MossFormer2_SE_48K.yaml`) and from the Kaldi filterbank definition its features come
//! from (`torchaudio.compliance.kaldi.fbank` defaults):
//!
//! - [`fbank`]: Kaldi log-mel filterbank, 60 bins, 40 ms Hamming frames every 8 ms (1920 / 384
//!   samples at 48 kHz), `snip_edges`, DC removal, pre-emphasis 0.97, FFT padded to 2048, power
//!   spectrum, triangular mel bins 20 Hz … Nyquist, `log(max(e, f32::EPSILON))`, dither 1.0 (16-bit
//!   units) from a fixed seed so results repeat;
//! - [`deltas`]: torchaudio's `compute_deltas` (window 5, edges replicated);
//! - [`features`]: the model's 180 input channels `[fbank, Δ, ΔΔ]` per frame;
//! - [`MaskStft`]: the 1920-point STFT / iSTFT (symmetric Hamming, hop 384, `center = false`) the
//!   predicted 961-bin mask multiplies (magnitude scaling, noisy phase).
//!
//! Every function checks its lengths and returns `None` instead of panicking; non-finite input
//! counts as silence.

use crate::fft::MixedFft;
use crate::testset::Rng;

/// Analysis window (40 ms at 48 kHz).
pub const WIN: usize = 1920;
/// Hop (8 ms at 48 kHz).
pub const HOP: usize = 384;
/// Mel bins.
pub const MELS: usize = 60;
/// Model input channels per frame: fbank, Δ, ΔΔ.
pub const FEATS: usize = 3 * MELS;
/// Mask / STFT bins (1920 / 2 + 1).
pub const BINS: usize = WIN / 2 + 1;
/// Kaldi rounds the frame up to a power of two for its FFT.
const KALDI_FFT: usize = 2048;
const SAMPLE_RATE: f64 = 48_000.0;
const LOW_HZ: f64 = 20.0;
const PREEMPH: f64 = 0.97;
/// Gaussian dither in 16-bit units (ClearerVoice passes `dither=1.0`).
pub const DITHER: f64 = 1.0;
/// Largest input handled in one call (≈ 6 h at 48 kHz): bounds every allocation here.
pub const MAX_SAMPLES: usize = 1 << 30;

fn mel(hz: f64) -> f64 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Kaldi's triangular mel filters, `[MELS][KALDI_FFT / 2 + 1]` (the Nyquist column is zero, as
/// torchaudio pads it).
pub fn mel_banks() -> Vec<[f64; KALDI_FFT / 2 + 1]> {
    let bins = KALDI_FFT / 2;
    let bin_hz = SAMPLE_RATE / KALDI_FFT as f64;
    let (lo, hi) = (mel(LOW_HZ), mel(SAMPLE_RATE / 2.0));
    let delta = (hi - lo) / (MELS + 1) as f64;
    (0..MELS)
        .map(|b| {
            let (left, center, right) = (lo + b as f64 * delta, lo + (b + 1) as f64 * delta, lo + (b + 2) as f64 * delta);
            let mut row = [0.0; KALDI_FFT / 2 + 1];
            for (k, w) in row.iter_mut().take(bins).enumerate() {
                let m = mel(bin_hz * k as f64);
                let up = (m - left) / (center - left);
                let down = (right - m) / (right - center);
                *w = up.min(down).max(0.0);
            }
            row
        })
        .collect()
}

/// Frames Kaldi (`snip_edges`) and the `center = false` STFT make from `n` samples.
pub fn frames(n: usize) -> usize {
    if n < WIN { 0 } else { 1 + (n - WIN) / HOP }
}

/// Symmetric Hamming window of `n` points (`torch.hamming_window(n, periodic=False)`, Kaldi's
/// `hamming`): 0.54 − 0.46 cos(2πi / (n − 1)).
pub fn hamming(n: usize) -> Vec<f64> {
    let d = n.saturating_sub(1).max(1) as f64;
    (0..n).map(|i| 0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / d).cos()).collect()
}

/// One standard-normal value (Box–Muller).
fn gauss(r: &mut Rng) -> f64 {
    let u1 = r.next_f64().max(1e-300);
    let u2 = r.next_f64();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Kaldi log-mel filterbank of `x` (16-bit scale, i.e. ±32768 full scale): `frames(len)` rows
/// of [`MELS`] values, row-major. `dither` is the Gaussian dither's standard deviation (0 = none),
/// drawn from `seed`.
pub fn fbank(x: &[f32], dither: f64, seed: u64) -> Option<Vec<f32>> {
    if x.len() > MAX_SAMPLES {
        return None;
    }
    let t = frames(x.len());
    let mut out = Vec::new();
    out.try_reserve_exact(t.checked_mul(MELS)?).ok()?;
    let banks = mel_banks();
    let win = hamming(WIN);
    let mut fft = MixedFft::new(KALDI_FFT)?;
    let (mut re, mut im) = (vec![0f32; KALDI_FFT], vec![0f32; KALDI_FFT]);
    let mut frame = vec![0f64; WIN];
    let mut power = vec![0f64; KALDI_FFT / 2 + 1];
    let mut rng = Rng::new(seed);
    for f in 0..t {
        let src = x.get(f * HOP..f * HOP + WIN)?;
        for (d, s) in frame.iter_mut().zip(src) {
            let v = if s.is_finite() { f64::from(*s) } else { 0.0 };
            *d = if dither > 0.0 { v + gauss(&mut rng) * dither } else { v };
        }
        let mean = frame.iter().sum::<f64>() / WIN as f64;
        frame.iter_mut().for_each(|v| *v -= mean);
        // pre-emphasis with the first sample replicated
        for i in (1..WIN).rev() {
            let prev = frame.get(i - 1).copied().unwrap_or(0.0);
            if let Some(v) = frame.get_mut(i) {
                *v -= PREEMPH * prev;
            }
        }
        if let Some(v) = frame.first_mut() {
            *v -= PREEMPH * *v;
        }
        re.iter_mut().for_each(|v| *v = 0.0);
        im.iter_mut().for_each(|v| *v = 0.0);
        for ((r, v), w) in re.iter_mut().zip(&frame).zip(&win) {
            *r = (v * w) as f32;
        }
        if !fft.forward(&mut re, &mut im) {
            return None;
        }
        for (k, p) in power.iter_mut().enumerate() {
            let (a, b) = (f64::from(*re.get(k)?), f64::from(*im.get(k)?));
            *p = a * a + b * b;
        }
        for row in &banks {
            let e: f64 = row.iter().zip(&power).map(|(w, p)| w * p).sum();
            out.push(e.max(f64::from(f32::EPSILON)).ln() as f32);
        }
    }
    Some(out)
}

/// torchaudio's `compute_deltas` (window 5, replicated edges) along time of `x`
/// (`t` rows × `c` columns, row-major).
pub fn deltas(x: &[f32], t: usize, c: usize) -> Option<Vec<f32>> {
    if x.len() != t.checked_mul(c)? {
        return None;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(x.len()).ok()?;
    let last = t.saturating_sub(1) as isize;
    for i in 0..t as isize {
        for j in 0..c {
            let mut acc = 0f64;
            for k in -2isize..=2 {
                let r = (i + k).clamp(0, last) as usize;
                acc += k as f64 * f64::from(*x.get(r * c + j)?);
            }
            out.push((acc / 10.0) as f32);
        }
    }
    Some(out)
}

/// The model's input for `x` (16-bit scale): `frames(len)` rows of [`FEATS`] values
/// (`[fbank, Δ, ΔΔ]`), row-major, and the frame count.
pub fn features(x: &[f32], dither: f64, seed: u64) -> Option<(Vec<f32>, usize)> {
    let fb = fbank(x, dither, seed)?;
    let t = fb.len() / MELS;
    let d1 = deltas(&fb, t, MELS)?;
    let d2 = deltas(&d1, t, MELS)?;
    let mut out = Vec::new();
    out.try_reserve_exact(t.checked_mul(FEATS)?).ok()?;
    for f in 0..t {
        let r = f * MELS..(f + 1) * MELS;
        out.extend_from_slice(fb.get(r.clone())?);
        out.extend_from_slice(d1.get(r.clone())?);
        out.extend_from_slice(d2.get(r)?);
    }
    Some((out, t))
}

/// The 1920 / 384 STFT with a symmetric Hamming window and no centring (`torch.stft` /
/// `torch.istft` as ClearerVoice calls them for this model).
pub struct MaskStft {
    fft: MixedFft,
    win: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl MaskStft {
    pub fn new() -> Option<MaskStft> {
        Some(MaskStft { fft: MixedFft::new(WIN)?, win: hamming(WIN).into_iter().map(|v| v as f32).collect(), re: vec![0.0; WIN], im: vec![0.0; WIN] })
    }

    /// Spectrum of `x`: `frames(len)` frames of [`BINS`] (re, im) pairs, frame-major.
    pub fn analyse(&mut self, x: &[f32]) -> Option<Vec<(f32, f32)>> {
        if x.len() > MAX_SAMPLES {
            return None;
        }
        let t = frames(x.len());
        let mut out = Vec::new();
        out.try_reserve_exact(t.checked_mul(BINS)?).ok()?;
        for f in 0..t {
            let src = x.get(f * HOP..f * HOP + WIN)?;
            for ((r, s), w) in self.re.iter_mut().zip(src).zip(&self.win) {
                *r = if s.is_finite() { s * w } else { 0.0 };
            }
            self.im.iter_mut().for_each(|v| *v = 0.0);
            if !self.fft.forward(&mut self.re, &mut self.im) {
                return None;
            }
            for k in 0..BINS {
                out.push((*self.re.get(k)?, *self.im.get(k)?));
            }
        }
        Some(out)
    }

    /// Overlap-add `spec` (as from [`MaskStft::analyse`]) back into `len` samples, dividing by the
    /// summed squared window; samples no frame covers are zero (as `torch.istft` pads them).
    pub fn synthesise(&mut self, spec: &[(f32, f32)], len: usize) -> Option<Vec<f32>> {
        if len > MAX_SAMPLES || !spec.len().is_multiple_of(BINS) {
            return None;
        }
        let t = spec.len() / BINS;
        let mut y = Vec::new();
        y.try_reserve_exact(len).ok()?;
        y.resize(len, 0f32);
        let mut env = vec![0f32; len];
        let scale = 1.0 / WIN as f32;
        for f in 0..t {
            let fr = spec.get(f * BINS..(f + 1) * BINS)?;
            // Hermitian spectrum; DC and Nyquist imaginary parts ignored (a real inverse)
            for k in 0..WIN {
                let (r, i) = if k < BINS {
                    let (r, i) = *fr.get(k)?;
                    (r, if k == 0 || k == WIN / 2 { 0.0 } else { i })
                } else {
                    let (r, i) = *fr.get(WIN - k)?;
                    (r, -i)
                };
                *self.re.get_mut(k)? = r;
                *self.im.get_mut(k)? = i;
            }
            if !self.fft.inverse(&mut self.re, &mut self.im) {
                return None;
            }
            let start = f * HOP;
            for n in 0..WIN {
                let Some(o) = y.get_mut(start + n) else { break };
                let w = *self.win.get(n)?;
                *o += self.re.get(n)? * scale * w;
                if let Some(e) = env.get_mut(start + n) {
                    *e += w * w;
                }
            }
        }
        for (v, e) in y.iter_mut().zip(&env) {
            *v = if *e > 1e-11 { *v / e } else { 0.0 };
        }
        Some(y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, n: usize, amp: f64) -> Vec<f32> {
        (0..n).map(|i| (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / SAMPLE_RATE).sin()) as f32).collect()
    }

    #[test]
    fn mel_filters_are_kaldi_triangles() {
        let b = mel_banks();
        assert_eq!(b.len(), MELS);
        for row in &b {
            let peak = row.iter().copied().fold(0.0, f64::max);
            assert!(peak > 0.5 && peak <= 1.0, "peak {peak}");
            assert_eq!(row[KALDI_FFT / 2], 0.0);
        }
        // first filter starts above 20 Hz (bin 0 = 0 Hz is outside every filter)
        assert!(b.iter().all(|r| r[0] == 0.0));
        // centres rise monotonically
        let centre = |r: &[f64; KALDI_FFT / 2 + 1]| r.iter().enumerate().fold((0, 0.0), |a, (k, v)| if *v > a.1 { (k, *v) } else { a }).0;
        let c: Vec<usize> = b.iter().map(centre).collect();
        assert!(c.windows(2).all(|w| w[1] > w[0]), "{c:?}");
    }

    #[test]
    fn frame_counts_follow_snip_edges() {
        assert_eq!(frames(0), 0);
        assert_eq!(frames(WIN - 1), 0);
        assert_eq!(frames(WIN), 1);
        assert_eq!(frames(WIN + HOP - 1), 1);
        assert_eq!(frames(WIN + HOP), 2);
        assert_eq!(fbank(&vec![0.0; WIN - 1], 0.0, 1).unwrap().len(), 0);
    }

    #[test]
    fn a_tone_lights_its_mel_bin() {
        let banks = mel_banks();
        for hz in [300.0, 1000.0, 4000.0, 12_000.0] {
            let x = sine(hz, 48_000, 8000.0);
            let fb = fbank(&x, 0.0, 1).unwrap();
            let row = &fb[10 * MELS..11 * MELS];
            let loud = row.iter().enumerate().fold((0, f32::MIN), |a, (i, v)| if *v > a.1 { (i, *v) } else { a }).0;
            // the bin whose filter weighs the tone's FFT bin most
            let k = (hz / (SAMPLE_RATE / KALDI_FFT as f64)).round() as usize;
            let want = (0..MELS).fold((0, 0.0), |a, b| if banks[b][k] > a.1 { (b, banks[b][k]) } else { a }).0;
            assert!(loud.abs_diff(want) <= 1, "{hz} Hz: bin {loud}, expected {want}");
        }
    }

    #[test]
    fn level_shifts_log_energy_by_20_log_ratio() {
        // log power: doubling the amplitude adds ln 4 to every (non-floored) bin
        let x = sine(1000.0, 24_000, 1000.0);
        let y: Vec<f32> = x.iter().map(|v| v * 2.0).collect();
        let (a, b) = (fbank(&x, 0.0, 1).unwrap(), fbank(&y, 0.0, 1).unwrap());
        let d = b[5 * MELS + 20] - a[5 * MELS + 20];
        assert!((d - 4f32.ln()).abs() < 1e-3, "{d}");
    }

    #[test]
    fn silence_floors_at_epsilon_and_dither_lifts_it() {
        let z = vec![0.0f32; 9600];
        let fb = fbank(&z, 0.0, 1).unwrap();
        assert!(fb.iter().all(|v| (*v - f32::EPSILON.ln()).abs() < 1e-4));
        let d = fbank(&z, DITHER, 1).unwrap();
        assert!(d.iter().all(|v| *v > f32::EPSILON.ln() + 5.0 && v.is_finite()));
        // the same seed repeats
        assert_eq!(d, fbank(&z, DITHER, 1).unwrap());
        // non-finite input counts as silence
        let mut bad = z.clone();
        bad[100] = f32::NAN;
        bad[200] = f32::INFINITY;
        assert_eq!(fbank(&bad, 0.0, 1).unwrap(), fb);
    }

    #[test]
    fn deltas_match_the_regression_formula() {
        // a ramp has slope 1 inside, and edge-replicated slope at the ends
        let t = 8;
        let x: Vec<f32> = (0..t).map(|i| i as f32).collect();
        let d = deltas(&x, t, 1).unwrap();
        assert!((d[3] - 1.0).abs() < 1e-6);
        assert!((d[0] - 0.5).abs() < 1e-6, "{}", d[0]); // (1·1 + 2·2 − 0 − 0) / 10
        assert!((d[1] - 0.8).abs() < 1e-6, "{}", d[1]); // (2 + 2·3 − 0 − 0) / 10
        assert!(deltas(&x, t, 2).is_none());
        assert_eq!(deltas(&[], 0, 3).unwrap().len(), 0);
        let (f, n) = features(&sine(440.0, 9600, 3000.0), 0.0, 1).unwrap();
        assert_eq!(f.len(), n * FEATS);
        assert_eq!(n, frames(9600));
    }

    /// Parity harness: features (no dither) of the raw little-endian f32 file
    /// `FILMCRAFT_FBANK_IN` (16-bit scale) written to `FILMCRAFT_FBANK_OUT`, for comparison with an
    /// independent Kaldi / torchaudio implementation (A3.7: numpy oracle written from the
    /// definitions: max abs error 1.2e-5 on fbank, 3.4e-6 on Δ, 1.5e-6 on ΔΔ).
    #[test]
    #[ignore]
    fn fbank_oracle_dump() {
        let (Some(i), Some(o)) = (std::env::var_os("FILMCRAFT_FBANK_IN"), std::env::var_os("FILMCRAFT_FBANK_OUT")) else {
            eprintln!("FILMCRAFT_FBANK_IN / _OUT unset: skipped");
            return;
        };
        let b = std::fs::read(i).unwrap();
        let x: Vec<f32> = b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        let (f, _) = features(&x, 0.0, 1).unwrap();
        std::fs::write(o, f.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    }

    #[test]
    fn stft_round_trip_is_exact_where_frames_cover() {
        let mut s = MaskStft::new().unwrap();
        let n = WIN + 40 * HOP;
        let x: Vec<f32> = (0..n).map(|i| ((i * 7919 % 1000) as f32 / 500.0 - 1.0) * 0.5).collect();
        let spec = s.analyse(&x).unwrap();
        assert_eq!(spec.len(), frames(n) * BINS);
        let y = s.synthesise(&spec, n + 100).unwrap();
        let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 1e-4, "{err}");
        assert!(y[n..].iter().all(|v| *v == 0.0));
        assert!(s.synthesise(&spec[1..], n).is_none());
    }
}
