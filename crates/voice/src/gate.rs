//! Noise-level gating for voice isolation (fork FORK-PLAN §1a.125 (b), AUDIO-PLAN A3.7): how
//! strongly to apply a voice model, chosen from how noisy the clip is. An already clean recording
//! gets light or no processing (a voice model always changes clean speech a little, and on a clean
//! clip that change is all there is); a noisy one gets the full model.
//!
//! The strength is DeepFilterNet's *attenuation limit*: the most the model may take away, in dB.
//! The output is `w · model + (1 − w) · input` with `w = 1 − 10^(−limit / 20)`, so the input stays
//! at least `limit` dB underneath ([`FULL`] = 100 dB: the model alone; [`BYPASS`] = 0 dB: the input
//! unchanged). The live DeepFilterNet3 effect applies it as its `limit` parameter; a rendered model
//! (MossFormer2) is mixed with [`apply_limit`]. Because the model's mask is applied to the input's
//! own spectrum, mixing the outputs is the same as limiting the mask (`m' = w · m + 1 − w`).
//!
//! The noise level is the speech-to-noise ratio the model itself hears ([`kept_removed_snr_db`]:
//! energy it keeps over energy it removes, at full strength), which also catches clicks and gusts
//! between words; when that isn't available, the frame-level estimate [`frame_snr_db`] (loud vs
//! quiet 20 ms frames), the same measure as Clean Up Voice's analysis. Pure Rust, no model needed.

/// No limit: the model's output alone.
pub const FULL: f32 = 100.0;
/// The input unchanged.
pub const BYPASS: f32 = 0.0;
/// At or below this speech-to-noise ratio (dB) the model runs at full strength.
pub const FULL_BELOW_DB: f64 = 25.0;
/// At or above this ratio (dB) the clip is left as it is.
pub const BYPASS_ABOVE_DB: f64 = 40.0;
/// The limit just above [`FULL_BELOW_DB`] (dB); it falls linearly to [`MIN_LIMIT_DB`] at
/// [`MIN_LIMIT_AT_DB`] and stays there until [`BYPASS_ABOVE_DB`]. (12 dB at 30 dB SNR, the limit
/// the A3.6 LibriSpeech sweep found best for mild noise.)
pub const MAX_LIMIT_DB: f64 = 18.0;
/// The lightest processing before bypass (dB; the live effect's smallest limit).
pub const MIN_LIMIT_DB: f64 = 6.0;
/// Where the limit reaches [`MIN_LIMIT_DB`] (dB SNR).
pub const MIN_LIMIT_AT_DB: f64 = 35.0;
/// Frames quieter than this (dBFS) are digital silence, not the noise floor.
const SILENCE_DB: f64 = -100.0;
/// At least this many 20 ms frames (0.5 s) of sound to estimate a level.
const MIN_FRAMES: usize = 25;

/// The attenuation limit (dB) for a measured speech-to-noise ratio (dB): [`FULL`] at or below
/// [`FULL_BELOW_DB`] (and for a non-finite ratio, so an unmeasurable clip still gets cleaned),
/// [`BYPASS`] at or above [`BYPASS_ABOVE_DB`], in between from [`MAX_LIMIT_DB`] down to
/// [`MIN_LIMIT_DB`] (reached at [`MIN_LIMIT_AT_DB`]) as the clip gets cleaner.
pub fn limit_for_snr(snr_db: f64) -> f32 {
    if !snr_db.is_finite() || snr_db <= FULL_BELOW_DB {
        return FULL;
    }
    if snr_db >= BYPASS_ABOVE_DB {
        return BYPASS;
    }
    let t = ((snr_db - FULL_BELOW_DB) / (MIN_LIMIT_AT_DB - FULL_BELOW_DB)).min(1.0);
    (MAX_LIMIT_DB + (MIN_LIMIT_DB - MAX_LIMIT_DB) * t) as f32
}

/// The model's share of the output for attenuation limit `limit_db`: 1 at [`FULL`] (or above),
/// 0 at [`BYPASS`] (or below, or non-finite).
pub fn wet_share(limit_db: f32) -> f32 {
    if limit_db >= FULL {
        1.0
    } else if limit_db.is_finite() && limit_db > 0.0 {
        1.0 - 10f32.powf(-limit_db / 20.0)
    } else {
        0.0
    }
}

/// `w · model + (1 − w) · input` in place in `model` (`w` = [`wet_share`]); `input` and `model`
/// are aligned samples (extra samples on either side are left as they are). Non-finite samples
/// count as silence.
pub fn apply_limit(input: &[f32], model: &mut [f32], limit_db: f32) {
    let w = wet_share(limit_db);
    if w >= 1.0 {
        return;
    }
    for (y, x) in model.iter_mut().zip(input) {
        let x = if x.is_finite() { *x } else { 0.0 };
        let m = if y.is_finite() { *y } else { 0.0 };
        *y = w * m + (1.0 - w) * x;
    }
}

fn db10(e: f64) -> f64 {
    10.0 * e.max(1e-30).log10()
}

/// Speech-to-noise ratio (dB) as a model hears it: the energy of its output `model` over the
/// energy it removed from `input` (aligned samples). `None` when the output is silent; a model
/// that removed nothing gives a large finite value.
pub fn kept_removed_snr_db(input: &[f32], model: &[f32]) -> Option<f64> {
    let (mut kept, mut removed) = (0.0f64, 0.0f64);
    for (x, y) in input.iter().zip(model) {
        if x.is_finite() && y.is_finite() {
            let (x, y) = (f64::from(*x), f64::from(*y));
            kept += y * y;
            removed += (x - y) * (x - y);
        }
    }
    (kept > 1e-20).then(|| db10(kept) - db10(removed))
}

/// Speech level over the noise floor (dB) from 20 ms frame levels at `sr` Hz: the 95th over the
/// 10th percentile, digital silence ignored. `None` with less than half a second of sound.
pub fn frame_snr_db(x: &[f32], sr: u32) -> Option<f64> {
    snr_of_levels(frame_levels(x.iter().copied(), sr))
}

/// 20 ms frame levels (dBFS) of `samples` at `sr` Hz (a trailing partial frame is dropped).
fn frame_levels(samples: impl Iterator<Item = f32>, sr: u32) -> Vec<f64> {
    let frame = (sr / 50).max(1) as usize;
    let (mut acc, mut k, mut levels) = (0.0f64, 0usize, Vec::new());
    for v in samples {
        let v = if v.is_finite() { f64::from(v) } else { 0.0 };
        acc += v * v;
        k += 1;
        if k == frame {
            levels.push(db10(acc / frame as f64));
            (acc, k) = (0.0, 0);
        }
    }
    levels
}

fn snr_of_levels(levels: Vec<f64>) -> Option<f64> {
    let mut levels: Vec<f64> = levels.into_iter().filter(|l| *l > SILENCE_DB).collect();
    if levels.len() < MIN_FRAMES {
        return None;
    }
    levels.sort_by(f64::total_cmp);
    let last = levels.len() - 1;
    let at = |p: f64| levels.get((last as f64 * p).round() as usize).copied();
    Some(at(0.95)? - at(0.10)?)
}

/// The ratio the strength is chosen by: the model's own measurement when there is one, else the
/// frame-level estimate (`None` when neither is known: then [`limit_for_snr`] runs the full model).
pub fn noise_snr_db(model_snr: Option<f64>, frame_snr: Option<f64>) -> Option<f64> {
    model_snr.filter(|v| v.is_finite()).or(frame_snr.filter(|v| v.is_finite()))
}

/// Gate a model's output in place: measure how noisy `input` is from what the model removed
/// (falling back to the frame levels at `sr`), and mix the input back in by [`limit_for_snr`].
/// Several channels are measured on their mean (the models listen to the mean too) and all get the
/// same limit; `input` and `model` pair up channel by channel. Streams over the samples (no copies
/// of the audio). Returns (the ratio used, if any; the limit applied).
pub fn gate(input: &[Vec<f32>], model: &mut [Vec<f32>], sr: u32) -> (Option<f64>, f32) {
    let chans = input.len().min(model.len());
    let n = input.iter().zip(model.iter()).map(|(x, y)| x.len().min(y.len())).min().unwrap_or(0);
    let k = 1.0 / chans.max(1) as f64;
    let fin = |v: f32| if v.is_finite() { f64::from(v) } else { 0.0 };
    let (mut kept, mut removed) = (0.0f64, 0.0f64);
    let mean_in = (0..n).map(|i| {
        let (mut x, mut y) = (0.0f64, 0.0f64);
        for (a, b) in input.iter().zip(model.iter()).take(chans) {
            x += a.get(i).copied().map_or(0.0, fin);
            y += b.get(i).copied().map_or(0.0, fin);
        }
        let (x, y) = (x * k, y * k);
        kept += y * y;
        removed += (x - y) * (x - y);
        x as f32
    });
    let levels = frame_levels(mean_in, sr);
    let model_snr = (kept > 1e-20).then(|| db10(kept) - db10(removed));
    let snr = noise_snr_db(model_snr, snr_of_levels(levels));
    let limit = limit_for_snr(snr.unwrap_or(f64::NAN));
    for (x, y) in input.iter().zip(model.iter_mut()) {
        apply_limit(x, y, limit);
    }
    (snr, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testset::{Noise, mix_at_snr, noise, rms, si_sdr};

    /// A speech-like surrogate: voiced harmonics under a syllable envelope with pauses.
    fn speech(secs: f64) -> Vec<f32> {
        let n = (secs * 48_000.0) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                let syl = (std::f64::consts::PI * 4.0 * t).sin().max(0.0).powi(2) * if (t % 2.0) < 1.6 { 1.0 } else { 0.0 };
                let f0 = 120.0 + 20.0 * (2.0 * std::f64::consts::PI * 0.7 * t).sin();
                let v: f64 = (1..12).map(|h| (2.0 * std::f64::consts::PI * f0 * h as f64 * t).sin() / h as f64).sum();
                (0.1 * syl * v) as f32
            })
            .collect()
    }

    #[test]
    fn limit_falls_with_the_noise_level() {
        assert_eq!(limit_for_snr(0.0), FULL);
        assert_eq!(limit_for_snr(FULL_BELOW_DB), FULL);
        assert_eq!(limit_for_snr(BYPASS_ABOVE_DB), BYPASS);
        assert_eq!(limit_for_snr(80.0), BYPASS);
        assert!((limit_for_snr(FULL_BELOW_DB + 1e-9) - MAX_LIMIT_DB as f32).abs() < 1e-3);
        assert!((limit_for_snr(30.0) - 12.0).abs() < 1e-4);
        assert_eq!(limit_for_snr(38.0), MIN_LIMIT_DB as f32);
        let mut last = f32::MAX;
        for k in 1..30 {
            let l = limit_for_snr(FULL_BELOW_DB + f64::from(k) * 0.5);
            assert!(l <= last && l >= MIN_LIMIT_DB as f32, "{k}: {l}");
            last = l;
        }
        // unmeasurable: clean it fully rather than skip it
        assert_eq!(limit_for_snr(f64::NAN), FULL);
        assert_eq!(limit_for_snr(f64::INFINITY), FULL);
        assert_eq!(limit_for_snr(f64::NEG_INFINITY), FULL);
    }

    #[test]
    fn wet_share_and_mixing() {
        assert_eq!(wet_share(FULL), 1.0);
        assert_eq!(wet_share(1e9), 1.0);
        assert_eq!(wet_share(BYPASS), 0.0);
        assert_eq!(wet_share(-5.0), 0.0);
        assert_eq!(wet_share(f32::NAN), 0.0);
        assert!((wet_share(20.0) - 0.9).abs() < 1e-6);
        let x = [1.0f32, -1.0, 0.5, f32::NAN];
        let mut y = [0.0f32, 0.0, f32::INFINITY, 0.25];
        apply_limit(&x, &mut y, 20.0);
        assert!((y[0] - 0.1).abs() < 1e-6 && (y[1] + 0.1).abs() < 1e-6);
        assert!((y[2] - 0.05).abs() < 1e-6, "non-finite model sample counts as silence");
        assert!((y[3] - 0.225).abs() < 1e-6, "non-finite input sample counts as silence");
        let mut z = [0.3f32; 2];
        apply_limit(&x, &mut z, FULL);
        assert_eq!(z, [0.3; 2], "full strength leaves the model's output alone");
        let mut b = [9.0f32; 3];
        apply_limit(&[1.0, 2.0], &mut b, BYPASS);
        assert_eq!(b, [1.0, 2.0, 9.0], "bypass: the input (only where both exist)");
    }

    #[test]
    fn snr_estimates() {
        let s = speech(6.0);
        let removed_a_tenth: Vec<f32> = s.iter().map(|v| v * 0.9).collect();
        let r = kept_removed_snr_db(&s, &removed_a_tenth).unwrap();
        assert!((r - 19.08).abs() < 0.1, "{r}");
        assert!(kept_removed_snr_db(&s, &s).unwrap() > 200.0);
        assert_eq!(kept_removed_snr_db(&s, &vec![0.0; s.len()]), None);
        assert_eq!(kept_removed_snr_db(&[], &[]), None);
        assert_eq!(kept_removed_snr_db(&[f32::NAN; 4], &[f32::INFINITY; 4]), None);
        // frame levels: a noise floor 30 dB under the loud frames
        let nz = noise(Noise::Fan, s.len(), 48_000, 3);
        let floor: Vec<f32> = nz.iter().map(|v| v * 0.1 * 10f32.powf(-30.0 / 20.0)).collect();
        let noisy: Vec<f32> = s.iter().zip(&floor).map(|(a, b)| a + b).collect();
        let f = frame_snr_db(&noisy, 48_000).unwrap();
        assert!(f > 20.0 && f < 60.0, "{f}");
        assert!(frame_snr_db(&s[..48_000 / 4], 48_000).is_none(), "under half a second");
        assert!(frame_snr_db(&vec![0.0; 96_000], 48_000).is_none(), "digital silence");
        assert!(frame_snr_db(&[f32::NAN; 96_000], 48_000).is_none());
        assert!(frame_snr_db(&noisy, 0).is_some(), "a zero rate doesn't divide by zero");
        assert_eq!(noise_snr_db(Some(f64::NAN), Some(12.0)), Some(12.0));
        assert_eq!(noise_snr_db(Some(30.0), Some(12.0)), Some(30.0));
        assert_eq!(noise_snr_db(None, None), None);
    }

    /// Clean speech that a model barely changes stays (near) identical to the input; a noisy mix
    /// that the model really cleans gets the model's full output.
    #[test]
    fn clean_speech_is_left_alone_and_noise_gets_the_full_model() {
        let s = speech(8.0);
        // a "model" that slightly alters clean speech (a 1 % error 40 dB down, plus a 0.3 dB dip)
        let y: Vec<f32> = s.iter().enumerate().map(|(i, v)| v * 0.966 + 0.001 * ((i as f32) * 0.37).sin()).collect();
        let before = si_sdr(&y, &s);
        let mut yy = vec![y];
        let (snr, limit) = gate(std::slice::from_ref(&s), &mut yy, 48_000);
        let y = yy.remove(0);
        assert!(snr.unwrap() > BYPASS_ABOVE_DB - 15.0, "{snr:?}");
        assert!(limit < FULL, "{limit}");
        assert!(si_sdr(&y, &s) >= before, "gating never makes clean speech worse");
        let mut exact = vec![s.clone()];
        let (_, l2) = gate(std::slice::from_ref(&s), &mut exact, 48_000);
        assert_eq!(l2, BYPASS);
        assert_eq!(exact[0], s);
        // a noisy mix at 5 dB SNR and a perfect "model" that returns the clean speech
        let noisy = mix_at_snr(&s, &noise(Noise::Traffic, s.len(), 48_000, 5), 5.0);
        let mut out = vec![s.clone(), s.clone()];
        let (snr, limit) = gate(&[noisy.clone(), noisy], &mut out, 48_000);
        assert!(snr.unwrap() < FULL_BELOW_DB, "{snr:?}");
        assert_eq!(limit, FULL);
        assert_eq!(out[1], s, "the full model's output is untouched");
        assert!((rms(&out[0]) - rms(&s)).abs() < 1e-9);
    }

    #[test]
    fn hostile_inputs_never_panic() {
        assert_eq!(gate(&[], &mut [], 48_000), (None, FULL));
        assert_eq!(gate(&[vec![]], &mut [vec![]], 48_000), (None, FULL));
        let mut y = vec![vec![f32::NAN; 1000]];
        let (_, l) = gate(&[vec![f32::INFINITY; 10]], &mut y, 0);
        assert_eq!(l, FULL);
        // mismatched lengths and channel counts
        let mut short = vec![vec![0.5f32; 3], vec![0.1; 7]];
        let _ = gate(&[vec![1.0; 100]], &mut short, u32::MAX);
        let mut huge = vec![vec![f32::MAX; 4800]];
        let _ = gate(&[vec![f32::MIN; 4800]], &mut huge, 48_000);
        assert!(huge[0].iter().all(|v| !v.is_nan()));
    }
}
