//! Measured picture facts for the assistant (fork AI-PLAN AI4.1b): what a frame looks like,
//! measured the way the scopes would read it, as a compact plain-language fact sheet the
//! assistant model is given with a question. Exposure (clipped / crushed share, where the mids
//! sit against middle grey, contrast), the cast of near-neutral areas (plain words and a
//! correlated colour temperature, McCamy's approximation [Standard]), saturation and skin
//! against the skin line (the centre of [`color_match::skin_weight`]'s hue window).
//!
//! Frames are working linear light (premultiplied RGBA, 1.0 = reference white), converted to
//! Rec. 709 primaries for measuring. Every number is checked: non-finite or transparent pixels
//! are skipped, at most [`MAX_SAMPLES`] pixels are read, and an empty frame gives `None`.

use filmcraft_color::spaces::{apply3, gamut_matrix, to_f32};
use filmcraft_color::{Gamut, WorkingSpace};

use crate::Image;
use crate::color_match::{oklab, skin_weight};

/// At most this many pixels are read (a regular stride over larger frames).
pub const MAX_SAMPLES: usize = 1 << 20;
/// A channel at or above this (working linear) counts as clipped.
pub const CLIP_LEVEL: f32 = 0.99;
/// Every channel at or below this (working linear) counts as crushed.
pub const CRUSH_LEVEL: f32 = 0.002;
/// Middle grey (linear luminance).
pub const MIDDLE_GREY: f32 = 0.18;
/// Near-neutral pixels: Oklab chroma below this, lightness inside [`NEUTRAL_L`].
pub const NEUTRAL_CHROMA: f32 = 0.04;
/// With a strong cast nothing is that grey any more: when fewer than [`NEUTRAL_MIN_SHARE`] pass
/// [`NEUTRAL_CHROMA`], pixels below this chroma are used instead (the least colourful areas).
pub const NEUTRAL_CHROMA_WIDE: f32 = 0.1;
pub const NEUTRAL_L: (f32, f32) = (0.2, 0.95);
/// The skin line's Oklab hue (degrees): the centre of the skin-weight window (20–75°).
pub const SKIN_LINE_DEG: f32 = 47.5;
/// Skin is described only when at least this share of the frame looks like skin.
pub const SKIN_MIN_SHARE: f32 = 0.02;
/// The cast is described only when this share of the frame is near-neutral.
pub const NEUTRAL_MIN_SHARE: f32 = 0.02;
/// Just-noticeable cast (Oklab a / b units), as Shot Match uses.
const JND_CAST: f32 = crate::grade_match::JND_CAST;

/// One frame's measured facts.
#[derive(Clone, Debug, PartialEq)]
pub struct PictureFacts {
    /// Share of opaque pixels with a channel at [`CLIP_LEVEL`] or above.
    pub clipped: f32,
    /// Share of opaque pixels with every channel at [`CRUSH_LEVEL`] or below.
    pub crushed: f32,
    /// Median luminance against middle grey, in stops (negative: darker).
    pub mids_stops: f32,
    /// 95th against 5th percentile luminance, in stops.
    pub contrast_stops: f32,
    /// Share of opaque pixels that are near-neutral.
    pub neutral_share: f32,
    /// Mean Oklab (a, b) of the near-neutral pixels (0 when there are none).
    pub cast: [f32; 2],
    /// Correlated colour temperature of the near-neutral pixels' mean colour (K), when measurable.
    pub neutral_cct: Option<f32>,
    /// Mean Oklab chroma of the opaque pixels.
    pub chroma: f32,
    /// Share of opaque pixels that look like skin (summed skin weight).
    pub skin_share: f32,
    /// Mean skin hue minus [`SKIN_LINE_DEG`] (positive: towards yellow), when skin is present.
    pub skin_offset_deg: Option<f32>,
}

pub(crate) fn working_gamut(working: WorkingSpace) -> Gamut {
    match working {
        WorkingSpace::Rec709 => Gamut::Bt709,
        WorkingSpace::Rec2100Pq | WorkingSpace::Rec2100Hlg => Gamut::Bt2020,
        WorkingSpace::DaVinciWide => Gamut::DaVinciWide,
    }
}

/// Luminance of Rec. 709-primaries linear RGB.
pub(crate) fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// McCamy's CCT approximation from Rec. 709 linear RGB (XYZ via the Rec. 709 matrix). `None`
/// outside its useful range (very saturated or dark colours).
fn mccamy_cct(c: [f32; 3]) -> Option<f32> {
    let [r, g, b] = c.map(f64::from);
    let x = 0.4124 * r + 0.3576 * g + 0.1805 * b;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = 0.0193 * r + 0.1192 * g + 0.9505 * b;
    let sum = x + y + z;
    if !(sum.is_finite() && sum > 1e-6) {
        return None;
    }
    let (cx, cy) = (x / sum, y / sum);
    let d = 0.1858 - cy;
    if d.abs() < 1e-9 {
        return None;
    }
    let n = (cx - 0.3320) / d;
    let cct = 449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33;
    (cct.is_finite() && (1500.0..=25_000.0).contains(&cct)).then_some(cct as f32)
}

/// The value at fraction `q` (0…1) of a sorted slice.
pub(crate) fn quantile(sorted: &[f32], q: f32) -> Option<f32> {
    let last = sorted.len().checked_sub(1)?;
    sorted.get(((last as f32) * q.clamp(0.0, 1.0)).round() as usize).copied()
}

impl PictureFacts {
    /// Measure a frame in `working`'s linear light. `None` for an empty, fully transparent or
    /// non-finite frame (or a buffer shorter than `w × h × 4`).
    pub fn measure(img: &Image, working: WorkingSpace) -> Option<PictureFacts> {
        let total = img.w.checked_mul(img.h)?;
        let px = img.px.get(..total.checked_mul(4)?)?;
        if total == 0 {
            return None;
        }
        let g = working_gamut(working);
        let m = (g != Gamut::Bt709).then(|| to_f32(&gamut_matrix(g, Gamut::Bt709)));
        let stride = total.div_ceil(MAX_SAMPLES).max(1);
        let (mut clipped, mut crushed) = (0f64, 0f64);
        // [strict, wide]: count, Oklab (a, b) sums, linear RGB sums
        let mut tiers = [(0f64, [0f64; 2], [0f64; 3]); 2];
        let (mut chroma, mut skin, mut skin_hue) = (0f64, 0f64, 0f64);
        let mut lumas = Vec::with_capacity(total.div_ceil(stride).min(MAX_SAMPLES));
        for p in px.as_chunks::<4>().0.iter().step_by(stride) {
            if !p.iter().all(|v| v.is_finite()) || p[3] <= 1e-3 {
                continue;
            }
            let a = p[3].min(1.0);
            let mut c = [p[0] / a, p[1] / a, p[2] / a].map(|v| v.clamp(-1e6, 1e6));
            if let Some(m) = &m {
                c = apply3(m, c);
            }
            if !c.iter().all(|v| v.is_finite()) {
                continue;
            }
            if c.iter().any(|v| *v >= CLIP_LEVEL) {
                clipped += 1.0;
            }
            if c.iter().all(|v| *v <= CRUSH_LEVEL) {
                crushed += 1.0;
            }
            lumas.push(luma(c).max(0.0));
            let lab = oklab(c);
            let ch = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            if !(lab[0].is_finite() && ch.is_finite()) {
                continue;
            }
            chroma += ch as f64;
            if (NEUTRAL_L.0..=NEUTRAL_L.1).contains(&lab[0]) {
                for (tier, limit) in tiers.iter_mut().zip([NEUTRAL_CHROMA, NEUTRAL_CHROMA_WIDE]) {
                    if ch < limit {
                        tier.0 += 1.0;
                        tier.1[0] += lab[1] as f64;
                        tier.1[1] += lab[2] as f64;
                        for (n, v) in tier.2.iter_mut().zip(c) {
                            *n += v.clamp(0.0, 1e3) as f64;
                        }
                    }
                }
            }
            let w = skin_weight(lab);
            if w > 0.0 {
                skin += w as f64;
                skin_hue += (w * lab[2].atan2(lab[1]).to_degrees()) as f64;
            }
        }
        let n = lumas.len();
        if n == 0 {
            return None;
        }
        lumas.sort_by(f32::total_cmp);
        let floor = 1e-4f32;
        let median = quantile(&lumas, 0.5)?.max(floor);
        let (lo, hi) = (quantile(&lumas, 0.05)?.max(floor), quantile(&lumas, 0.95)?.max(floor));
        let nf = n as f64;
        let [strict, wide] = tiers;
        let (neutral, cast, neutral_rgb) = if strict.0 / nf >= f64::from(NEUTRAL_MIN_SHARE) { strict } else { wide };
        let share = |v: f64| (v / nf) as f32;
        let mean_neutral = neutral_rgb.map(|v| (v / neutral.max(1.0)) as f32);
        let out = PictureFacts {
            clipped: share(clipped),
            crushed: share(crushed),
            mids_stops: (median / MIDDLE_GREY).log2(),
            contrast_stops: (hi / lo).log2(),
            neutral_share: share(neutral),
            cast: if neutral > 0.0 { cast.map(|v| (v / neutral) as f32) } else { [0.0; 2] },
            neutral_cct: if neutral > 0.0 { mccamy_cct(mean_neutral) } else { None },
            chroma: share(chroma),
            skin_share: share(skin),
            skin_offset_deg: (skin > 0.0).then(|| (skin_hue / skin) as f32 - SKIN_LINE_DEG),
        };
        let finite =
            [out.clipped, out.crushed, out.mids_stops, out.contrast_stops, out.chroma, out.skin_share, out.cast[0], out.cast[1]].iter().all(|v| v.is_finite());
        finite.then_some(out)
    }

    /// The near-neutral areas' cast in plain words ("neutral", "slightly warm", "cool and a
    /// little green", …).
    pub fn cast_words(&self) -> String {
        let degree = |v: f32| {
            let a = v.abs();
            if a < JND_CAST {
                None
            } else if a < 3.0 * JND_CAST {
                Some("slightly ")
            } else if a < 7.5 * JND_CAST {
                Some("")
            } else {
                Some("very ")
            }
        };
        let [a, b] = self.cast;
        let warm = degree(b).map(|d| format!("{d}{}", if b > 0.0 { "warm (towards orange)" } else { "cool (towards blue)" }));
        let tint = degree(a).map(|d| format!("{d}{}", if a > 0.0 { "magenta" } else { "green" }));
        match (warm, tint) {
            (None, None) => "neutral".to_string(),
            (Some(w), None) => w,
            (None, Some(t)) => format!("{t} tint"),
            (Some(w), Some(t)) => format!("{w}, {t} tint"),
        }
    }

    /// The compact fact sheet the assistant is given (a few short lines).
    pub fn sheet(&self) -> String {
        let pct = |v: f32| (v * 100.0).round();
        let mut lines = Vec::new();
        let side = if self.mids_stops < 0.0 { "below" } else { "above" };
        lines.push(format!(
            "Exposure: mids about {:.1} stops {side} middle grey; {}% clipped highlights, {}% crushed blacks; contrast {:.1} stops (5–95 %).",
            self.mids_stops.abs(),
            pct(self.clipped),
            pct(self.crushed),
            self.contrast_stops
        ));
        if self.neutral_share >= NEUTRAL_MIN_SHARE {
            let k = self.neutral_cct.map(|k| format!(", about {} K", (k / 100.0).round() * 100.0)).unwrap_or_default();
            lines.push(format!("White balance: near-neutral areas ({}% of the frame) look {}{k}.", pct(self.neutral_share), self.cast_words()));
        } else {
            lines.push("White balance: too few near-neutral areas to judge.".to_string());
        }
        let sat = if self.chroma < 0.03 {
            "low (muted)"
        } else if self.chroma < 0.08 {
            "moderate"
        } else {
            "high"
        };
        lines.push(format!("Saturation: {sat}."));
        match self.skin_offset_deg.filter(|_| self.skin_share >= SKIN_MIN_SHARE) {
            Some(d) if d.abs() < 5.0 => {
                lines.push(format!("Skin-tone colours (faces, or any warm surface): {}% of the frame, on the skin line.", pct(self.skin_share)))
            }
            Some(d) => lines.push(format!(
                "Skin-tone colours (faces, or any warm surface): {}% of the frame, about {:.0}° {} the skin line.",
                pct(self.skin_share),
                d.abs(),
                if d > 0.0 { "towards yellow from" } else { "towards red / magenta from" }
            )),
            None => lines.push("Skin-tone colours: little or none in the frame.".to_string()),
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(c: [f32; 3]) -> Image {
        Image::filled(64, 36, [c[0], c[1], c[2], 1.0])
    }

    #[test]
    fn middle_grey_is_neutral_on_the_mark_and_about_6500_k() {
        let f = PictureFacts::measure(&flat([0.18; 3]), WorkingSpace::Rec709).expect("facts");
        assert!(f.mids_stops.abs() < 0.01 && f.contrast_stops.abs() < 0.01, "{f:?}");
        assert_eq!((f.clipped, f.crushed), (0.0, 0.0));
        assert!((f.neutral_share - 1.0).abs() < 1e-6 && f.cast_words() == "neutral", "{f:?}");
        let k = f.neutral_cct.expect("cct");
        assert!((6300.0..=6700.0).contains(&k), "{k}");
        assert!(f.sheet().contains("mids about 0.0 stops") && f.sheet().contains("look neutral"), "{}", f.sheet());
        // the same grey in DaVinci Wide Gamut measures the same
        let g = PictureFacts::measure(&flat([0.18; 3]), WorkingSpace::DaVinciWide).expect("facts");
        assert!(g.mids_stops.abs() < 0.01 && g.cast_words() == "neutral", "{g:?}");
    }

    #[test]
    fn a_warm_dark_clipped_frame_is_described_as_such() {
        let mut img = flat([0.05, 0.04, 0.025]);
        for p in img.px.as_chunks_mut::<4>().0.iter_mut().take(64 * 9) {
            p[..3].copy_from_slice(&[1.5, 1.4, 1.2]);
        }
        let f = PictureFacts::measure(&img, WorkingSpace::Rec709).expect("facts");
        assert!((f.clipped - 0.25).abs() < 0.01, "{f:?}");
        assert!(f.mids_stops < -1.5, "{f:?}");
        assert!(f.cast[1] > 0.0 && f.cast_words().contains("warm"), "{}", f.cast_words());
        assert!(f.neutral_cct.is_some_and(|k| k < 5500.0), "{f:?}");
        assert!(f.sheet().contains("25% clipped highlights") && f.sheet().contains("below middle grey"), "{}", f.sheet());
        // a cool frame goes the other way
        let c = PictureFacts::measure(&flat([0.15, 0.17, 0.22]), WorkingSpace::Rec709).expect("facts");
        assert!(c.cast_words().contains("cool") && c.neutral_cct.is_some_and(|k| k > 7000.0), "{c:?}");
    }

    #[test]
    fn skin_is_found_and_placed_against_the_skin_line() {
        // a typical light skin tone (linear Rec. 709)
        let f = PictureFacts::measure(&flat([0.45, 0.25, 0.17]), WorkingSpace::Rec709).expect("facts");
        assert!(f.skin_share > 0.5 && f.skin_offset_deg.is_some(), "{f:?}");
        assert!(f.sheet().contains("Skin-tone colours (faces, or any warm surface): "), "{}", f.sheet());
        let none = PictureFacts::measure(&flat([0.1, 0.3, 0.1]), WorkingSpace::Rec709).expect("facts");
        assert!(none.sheet().contains("little or none"), "{}", none.sheet());
    }

    #[test]
    fn hostile_frames_give_none_or_finite_facts() {
        assert_eq!(PictureFacts::measure(&Image::new(0, 0), WorkingSpace::Rec709), None);
        assert_eq!(PictureFacts::measure(&Image::new(8, 8), WorkingSpace::Rec709), None, "fully transparent");
        let short = Image { w: 100, h: 100, px: vec![0.5; 16] };
        assert_eq!(PictureFacts::measure(&short, WorkingSpace::Rec709), None, "short buffer");
        let huge = Image { w: usize::MAX, h: 2, px: vec![] };
        assert_eq!(PictureFacts::measure(&huge, WorkingSpace::Rec709), None, "overflowing size");
        let mut evil = flat([0.2; 3]);
        let vals = [f32::NAN, f32::INFINITY, -f32::INFINITY, f32::MAX, -1e30, 0.0];
        for (i, p) in evil.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            p[i % 4] = vals[i % vals.len()];
        }
        if let Some(f) = PictureFacts::measure(&evil, WorkingSpace::Rec2100Pq) {
            assert!(f.sheet().len() < 600 && !f.sheet().contains("NaN") && !f.sheet().contains("inf"), "{}", f.sheet());
        }
    }
}
