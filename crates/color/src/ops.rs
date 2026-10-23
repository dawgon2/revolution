//! Composable per-pixel colour operations (fork ADR 0003).
//!
//! A [`ColorOp`] is one small step of a colour pipeline: a 3×3 matrix with an offset (gamut
//! conversion, white balance, exposure, affine levels), a transfer function (signal ↔ linear), a
//! sampled 1D table (camera log decoding, curves), lift / gamma / gain / offset, saturation, …
//! Input transforms, grades (Lumetri) and output transforms are chains of them, run by the CPU
//! ([`apply_chain`], the reference) and by one fused GPU pass (`filmcraft_gpu`) alike, with parity
//! tests per operation. Everything here is `f32`, as on the GPU.

use std::sync::Arc;

use crate::log::{Domain, LogCurve, normalized_from_signal, signal_from_normalized};
use crate::spaces::{Gamut, Mat3};
use crate::{Range, hlg_inverse_oetf, hlg_oetf, linear_to_srgb, pq_eotf, pq_inverse_eotf, srgb_to_linear};

/// The largest GPU program (`f32` values) a chain may need ([`gpu_size`]); larger chains (a big
/// 3D LUT) stay on the CPU.
pub const MAX_GPU_PROGRAM: usize = 1 << 22;

/// Samples in a table made by [`Table::sample`].
pub const TABLE_SIZE: usize = 4097;

/// DaVinci Wide Gamut primaries (x, y; D65 white): Blackmagic Design, *DaVinci Resolve 17 Wide
/// Gamut Intermediate* (2021).
pub const DWG_PRIMARIES: [[f64; 2]; 3] = [[0.8000, 0.3130], [0.1682, 0.9877], [0.0790, -0.1155]];

/// DaVinci Intermediate constants (same document).
pub mod di {
    pub const A: f32 = 0.0075;
    pub const B: f32 = 7.0;
    pub const C: f32 = 0.073_292_48;
    // published 10.44426855 (the nearest f32)
    pub const M: f32 = 10.444_268;
    pub const LIN_CUT: f32 = 0.002_624_09;
    pub const LOG_CUT: f32 = 0.027_406_68;
}

/// Linear → DaVinci Intermediate.
pub fn di_encode(x: f32) -> f32 {
    if x <= di::LIN_CUT { x * di::M } else { ((x + di::A).log2() + di::B) * di::C }
}

/// DaVinci Intermediate → linear.
pub fn di_decode(y: f32) -> f32 {
    if y <= di::LOG_CUT { y / di::M } else { (y / di::C - di::B).exp2() - di::A }
}

/// Transfer functions with an analytic form on both the CPU and the GPU. Camera log curves are
/// decoded through a [`Table`] instead ([`ColorOp::decode_log`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tf {
    /// IEC 61966-2-1.
    Srgb,
    /// BT.1886 display gamma 2.4 (sign-preserving).
    Gamma24,
    /// SMPTE ST 2084, linear 1.0 = 10 000 cd/m².
    Pq,
    /// ARIB STD-B67 / BT.2100 HLG OETF (scene light 0…1).
    Hlg,
    /// DaVinci Intermediate.
    DaVinciIntermediate,
}

impl Tf {
    /// Signal → linear.
    pub fn decode(self, v: f32) -> f32 {
        match self {
            Tf::Srgb => srgb_to_linear(v),
            Tf::Gamma24 => v.abs().powf(2.4).copysign(v),
            Tf::Pq => pq_eotf(v),
            Tf::Hlg => hlg_inverse_oetf(v),
            Tf::DaVinciIntermediate => di_decode(v),
        }
    }
    /// Linear → signal.
    pub fn encode(self, x: f32) -> f32 {
        match self {
            Tf::Srgb => linear_to_srgb(x),
            Tf::Gamma24 => x.abs().powf(1.0 / 2.4).copysign(x),
            Tf::Pq => pq_inverse_eotf(x),
            Tf::Hlg => hlg_oetf(x),
            Tf::DaVinciIntermediate => di_encode(x),
        }
    }
    /// The GPU's code for it (`colorops.wgsl`).
    pub fn code(self) -> u32 {
        match self {
            Tf::Srgb => 0,
            Tf::Gamma24 => 1,
            Tf::Pq => 2,
            Tf::Hlg => 3,
            Tf::DaVinciIntermediate => 4,
        }
    }
}

/// A sampled 1D function: `values[i]` at `lo + i·(hi − lo)/(n − 1)`, linearly interpolated and
/// extended linearly beyond both ends with the end segments' slopes.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub lo: f32,
    pub hi: f32,
    pub values: Arc<[f32]>,
}

impl Table {
    /// `f` sampled at `n` points over `lo..=hi` (`n` at least 2).
    pub fn sample(lo: f32, hi: f32, n: usize, f: impl Fn(f64) -> f64) -> Table {
        let n = n.max(2);
        let step = (f64::from(hi) - f64::from(lo)) / (n - 1) as f64;
        let values: Vec<f32> = (0..n).map(|i| f(f64::from(lo) + i as f64 * step) as f32).collect();
        Table { lo, hi, values: values.into() }
    }

    /// The table at `x` (the GPU does the same arithmetic).
    pub fn eval(&self, x: f32) -> f32 {
        let n = self.values.len();
        let (Some(&first), true) = (self.values.first(), n >= 2) else {
            return self.values.first().copied().unwrap_or(x);
        };
        let span = self.hi - self.lo;
        if span.abs() <= f32::EPSILON {
            return first;
        }
        let t = (x - self.lo) / span * (n - 1) as f32;
        let i = t.floor().clamp(0.0, (n - 2) as f32);
        let fr = t - i;
        let i = i as usize;
        let (a, b) = (self.values.get(i).copied().unwrap_or(first), self.values.get(i + 1).copied().unwrap_or(first));
        a + (b - a) * fr
    }

    /// A tone curve over 0…1 at `x`: clamped to the curve's ends, or with `hdr` continued above
    /// 1 with slope 1 from its end point.
    pub fn eval_curve(&self, x: f32, hdr: bool) -> f32 {
        if hdr && x > 1.0 { self.eval(1.0) + (x - 1.0) } else { self.eval(x.clamp(0.0, 1.0)) }
    }
}

/// Lumetri's hue / saturation curves (each optional, 0.5 = neutral), on the HSL of the signal.
#[derive(Clone, Debug, PartialEq)]
pub struct HslCurves {
    pub hue_vs_hue: Option<Table>,
    pub hue_vs_sat: Option<Table>,
    pub hue_vs_luma: Option<Table>,
    pub luma_vs_sat: Option<Table>,
    pub sat_vs_sat: Option<Table>,
    /// The signal may exceed 1 (HDR): scaled into 0…1 and back instead of clamped.
    pub hdr: bool,
}

impl HslCurves {
    fn tables(&self) -> [&Option<Table>; 5] {
        [&self.hue_vs_hue, &self.hue_vs_sat, &self.hue_vs_luma, &self.luma_vs_sat, &self.sat_vs_sat]
    }

    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let s = |t: &Table, x: f32| t.eval_curve(x, false);
        within_unit(c, self.hdr, |u| {
            let mut h = crate::rgb_to_hsl(u[0], u[1], u[2]);
            let (h0, s0, l0) = (h[0], h[1], h[2]);
            if let Some(t) = &self.hue_vs_hue {
                h[0] = (h[0] + (s(t, h0) - 0.5)).rem_euclid(1.0);
            }
            let mut sm = 1.0;
            if let Some(t) = &self.hue_vs_sat {
                sm *= s(t, h0) * 2.0;
            }
            if let Some(t) = &self.luma_vs_sat {
                sm *= s(t, l0) * 2.0;
            }
            if let Some(t) = &self.sat_vs_sat {
                sm *= s(t, s0) * 2.0;
            }
            h[1] = (h[1] * sm).clamp(0.0, 1.0);
            if let Some(t) = &self.hue_vs_luma {
                h[2] = (h[2] + (s(t, h0) - 0.5) * 0.5).clamp(0.0, 1.0);
            }
            crate::hsl_to_rgb(h[0], h[1], h[2])
        })
    }
}

/// A signal brought into 0…1: clamped (SDR), or with `hdr` divided by its largest component
/// when that exceeds 1.
#[inline]
pub fn to_unit(v: [f32; 3], hdr: bool) -> [f32; 3] {
    if !hdr {
        return v.map(|q| q.clamp(0.0, 1.0));
    }
    let m = v[0].max(v[1]).max(v[2]).max(1.0);
    v.map(|q| (q / m).max(0.0))
}

/// Run `f`, defined on 0…1, on a signal: clamped to 0…1 (SDR), or with `hdr` scaled into 0…1 by
/// its largest component and back afterwards (highlights above 1 are kept, not clipped).
#[inline]
pub fn within_unit(v: [f32; 3], hdr: bool, f: impl Fn([f32; 3]) -> [f32; 3]) -> [f32; 3] {
    if !hdr {
        return f(v.map(|q| q.clamp(0.0, 1.0)));
    }
    let m = v[0].max(v[1]).max(v[2]).max(1.0);
    let o = f(v.map(|q| (q / m).max(0.0)));
    o.map(|q| q * m)
}

/// Lumetri's built-in procedural Looks (our own grades, no third-party LUTs) on a display-like
/// 0…1 signal; 0 and unknown numbers are the identity.
pub fn apply_look(look: u32, c: [f32; 3]) -> [f32; 3] {
    let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let mix = |a: [f32; 3], b: [f32; 3], k: f32| [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k];
    let s_curve = |v: f32, k: f32| {
        let x = v.clamp(0.0, 1.0);
        x + (x * x * (3.0 - 2.0 * x) - x) * k
    };
    match look {
        1 => {
            // teal shadows, orange highlights
            let shadow = [0.0, 0.08, 0.1];
            let high = [0.1, 0.04, -0.06];
            let c = [c[0] + shadow[0] * (1.0 - l) + high[0] * l, c[1] + shadow[1] * (1.0 - l) + high[1] * l, c[2] + shadow[2] * (1.0 - l) + high[2] * l];
            c.map(|v| s_curve(v, 0.35))
        }
        2 => {
            let c = [c[0] * 1.06 + 0.02, c[1] * 1.0 + 0.01, c[2] * 0.9];
            mix(c, [l, l, l], 0.15).map(|v| v * 0.94 + 0.04)
        }
        3 => [c[0] * 0.9, c[1] * 0.98, c[2] * 1.1 + 0.02].map(|v| s_curve(v, 0.2)),
        4 => mix(c, [l, l, l], 0.55).map(|v| s_curve(v, 0.6)),
        5 => mix(c, [l, l, l], 0.25).map(|v| 0.08 + v * 0.84),
        6 => [l, l, l].map(|v| s_curve(v, 0.3)),
        7 => [c[0] * 1.1 + 0.03, c[1] * 1.02 + 0.01, c[2] * 0.82].map(|v| s_curve(v, 0.15)),
        8 => [c[0] * 0.75, c[1] * 0.85, c[2] * 1.15].map(|v| v * 0.8),
        _ => c,
    }
}

/// Whether a LUT's tables are well formed for interpolation (sizes, data lengths, domains).
fn lut_ok(l: &crate::Lut) -> bool {
    let dom = |lo: &[f32; 3], hi: &[f32; 3]| (0..3).all(|k| lo[k].is_finite() && hi[k].is_finite() && hi[k] != lo[k]);
    let data = |d: &[[f32; 3]]| d.iter().flatten().all(|v| v.is_finite());
    l.shaper.as_ref().is_none_or(|s| s.data.len() >= 2 && dom(&s.domain_min, &s.domain_max) && data(&s.data))
        && l.cube.as_ref().is_none_or(|c| c.size >= 2 && c.size.checked_pow(3) == Some(c.data.len()) && dom(&c.domain_min, &c.domain_max) && data(&c.data))
}

/// One colour operation. The state it acts on is a pixel's `rgb`.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorOp {
    /// `rgb = m · rgb + offset`.
    Matrix { m: [[f32; 3]; 3], offset: [f32; 3] },
    /// Signal → linear, per channel.
    Decode(Tf),
    /// Linear → signal, per channel.
    Encode(Tf),
    /// The same sampled function on every channel.
    Table(Table),
    /// Lift / gamma / gain / offset per channel, on the signal: `a = lift + (gain − lift)·v`,
    /// `v' = sign(a)·|a|^(1/gamma) + offset`.
    Lggo { lift: [f32; 3], gamma: [f32; 3], gain: [f32; 3], offset: [f32; 3] },
    /// `rgb = l + (rgb − l)·s` around the luma `l = weights · rgb`, with
    /// `s = sat·(1 + vibrance·(1 − clamp(max − min, 0, 1)))`: vibrance raises the saturation of
    /// muted colours more than that of saturated ones.
    Saturation { sat: f32, vibrance: f32, weights: [f32; 3] },
    /// BT.2100 HLG OOTF in working units: `rgb · k·Ys^exponent`, `Ys = max(weights · rgb, 0)`.
    HlgOotf { weights: [f32; 3], k: f32, exponent: f32 },
    /// BT.2390 tone mapping on max(R, G, B) as a ratio (`transform::ToneMap`): `ratio` is sampled
    /// over the PQ signal of max(R, G, B) × 203 cd/m²; at or above `src_peak` cd/m² the ratio
    /// clips to `dst_peak`.
    ToneMap { src_peak: f32, dst_peak: f32, ratio: Table },
    /// Camera-gamut compression towards the achromatic axis (`transform::gamut_compress`).
    GamutCompress,
    /// Negative components desaturated away towards the luma (`transform::desaturate_into_gamut`).
    DesaturateIntoGamut { weights: [f32; 3] },
    /// Every channel clamped to `lo..=hi`.
    Clamp { lo: f32, hi: f32 },
    /// Lumetri Shadows / Highlights, hue preserving: the luma `l = weights · rgb` moves to
    /// `max(l + shadows·clamp(1 − l)³ + highlights·clamp(l)³, 0)` and `rgb` is scaled with it.
    ToneRegions { shadows: f32, highlights: f32, weights: [f32; 3] },
    /// Lumetri Contrast around mid grey on a 0…1 signal: `amount` > 0 blends towards the
    /// smoothstep S-curve, < 0 flattens towards 0.5. With `keep_above_one` (HDR) values above 1
    /// pass unchanged; otherwise the signal is clamped to 0…1 first.
    Contrast { amount: f32, keep_above_one: bool },
    /// HDR Specular: values above 1 (above HDR White) keep `gain` × their distance from 1.
    Specular { gain: f32 },
    /// Split toning: `rgb += shadows·(1 − l) + highlights·l` with `l = clamp(weights · rgb, 0, 1)`.
    SplitTone { shadows: [f32; 3], highlights: [f32; 3], weights: [f32; 3] },
    /// Lumetri Vignette at the pixel's position in a `size` image (pixel index, not centre):
    /// darkens (`amount` < 0) or lightens towards white beyond `midpoint`, over `feather`
    /// (both already ×0.9), the horizontal axis scaled by `sx` (roundness).
    Vignette { amount: f32, midpoint: f32, feather: f32, sx: f32, size: [f32; 2] },
    /// A tone curve ([`Table::eval_curve`]) on the channels marked in `channels` (R, G, B).
    Curve { table: Table, channels: [bool; 3], hdr: bool },
    /// Lumetri Colour Wheels on a 0…1 signal: with `l = clamp(weights · rgb, 0, 1)`, shadows
    /// weighted `(1 − l)²` and midtones `1 − (1 − l)² − l²` add their offsets, highlights
    /// weighted `l²` scale by `1 + highlights`.
    Wheels { shadows: [f32; 3], midtones: [f32; 3], highlights: [f32; 3], weights: [f32; 3] },
    /// Hue / saturation curves.
    HslCurves(Arc<HslCurves>),
    /// A LUT file (1D shaper, then a tetrahedral 3D cube) on the signal ([`within_unit`]), mixed
    /// in by `intensity`.
    Lut { lut: Arc<crate::Lut>, intensity: f32, hdr: bool },
    /// A procedural Look ([`apply_look`]), mixed in by `intensity`; with `hdr` it runs
    /// [`within_unit`].
    Look { look: u32, intensity: f32, hdr: bool },
    /// HSL Secondary key: sets the mask register from the HSL of the signal ([`within_unit`]):
    /// hue within `range` / 2 of `hue` (wrapping), saturation above `sat_min` (and below `sat_max`
    /// when it is under 1; 1 or more: no upper limit), lightness between `luma_min` and
    /// `luma_max`, each edge over `soft`. `rgb` is unchanged.
    HslKey { hue: f32, range: f32, sat_min: f32, sat_max: f32, luma_min: f32, luma_max: f32, soft: f32, hdr: bool },
    /// HSL Secondary correction inside the mask on the signal: hue shift, saturation,
    /// temperature, tint (`show_mask` 0), or a view of the mask (1: colour over grey, 2: colour
    /// over black, 3: the mask itself; display images, no HDR scaling).
    HslCorrect { hue_shift: f32, sat: f32, temp: f32, tint: f32, show_mask: u32, hdr: bool },
    /// Film grain (fork G1.6), monochrome, at the pixel's position: `rgb += strength ·
    /// GRAIN_AMPLITUDE · w(l) · n` with `n` = [`grain_noise`]`(xy, inv_size, softness, seed)` and
    /// the luma weighting `w(l) = GRAIN_FLOOR + (1 − GRAIN_FLOOR)·4l(1 − l)`,
    /// `l = clamp(weights · rgb, 0, 1)`: strongest in the mid-tones, faint in deep shadows and
    /// highlights, like film. The same `seed` always gives the same grain.
    Grain { strength: f32, inv_size: f32, softness: f32, seed: u32, weights: [f32; 3] },
    /// Keeps a copy of `rgb` in the stash register (fork G2.1: the start of a grade node limited
    /// by a qualifier). `rgb` and the mask are unchanged.
    Stash,
    /// Mixes `rgb` back towards the stash by the mask register: `rgb = stash + (rgb − stash)·m`
    /// with `m` = the mask (`1 − mask` when `invert`); the mask register becomes `m`. Ops between
    /// [`ColorOp::Stash`] and this one act only where the mask is set.
    MixStash { invert: bool },
    /// Colour warper (fork G4.1): a displacement field over hue × saturation or saturation ×
    /// luma, in HSV on the signal ([`crate::warp`] says exactly how).
    Warp2D(Arc<crate::warp::Warp2D>),
    /// 3D qualifier key (fork G4.3): sets the mask register from the pixel's distance to the
    /// include / exclude colour volumes ([`crate::key3d`] says exactly how). `rgb` is unchanged.
    Key3d(Arc<crate::key3d::Key3d>),
    /// Despill (fork G4.3): `rgb − amount · max(rgb · dir, 0) · dir`, `dir` a unit chroma
    /// direction ([`crate::key3d::despill`]).
    Despill { dir: [f32; 3], amount: f32 },
    /// HDR zone wheels (fork G4.2, [`ZoneWheels`]) on working linear light.
    ZoneWheels(Arc<ZoneWheels>),
}

/// Tonal zones of [`ZoneWheels`]; the first [`ZONE_DARK`] act below their edge, the rest above.
pub const ZONE_COUNT: usize = 6;
pub const ZONE_DARK: usize = 3;
/// Values of a [`ColorOp::ZoneWheels`] GPU program: code, weights, pivot, global, the zones.
pub const ZONE_PROGRAM_LEN: usize = 1 + 3 + 1 + 5 + ZONE_COUNT * 7;
/// The total exposure of all wheels a pixel gets is clamped to ± this many stops.
pub const ZONE_MAX_STOPS: f32 = 16.0;
/// Pixel luminances relative to the pivot are taken as at most ± this many stops (black, 0 and
/// negative luminances: the lowest).
pub const ZONE_STOP_LIMIT: f32 = 30.0;
/// The combined saturation a pixel gets is clamped to 0 … this.
pub const ZONE_MAX_SAT: f32 = 65_536.0;
/// The narrowest soft edge [`ZoneWheels`] draws, in stops.
pub const ZONE_MIN_FALLOFF: f32 = 0.05;
/// Output channels are clamped to ± this (finite even for absurd inputs and settings).
pub const ZONE_MAX_VALUE: f32 = 1e30;

/// What one HDR wheel does where it acts fully.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneAdjust {
    /// Exposure in stops: `rgb · 2^exposure`.
    pub exposure: f32,
    /// Saturation around the luminance (1: none).
    pub sat: f32,
    /// Colour balance: a luminance-neutral shift (`weights · shift` = 0), added as `Y · shift`.
    pub shift: [f32; 3],
}

impl ZoneAdjust {
    pub const IDENTITY: ZoneAdjust = ZoneAdjust { exposure: 0.0, sat: 1.0, shift: [0.0; 3] };
}

/// One tonal zone: where it acts and what it does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Zone {
    /// The zone's edge in stops from the pivot.
    pub edge: f32,
    /// The width of its soft edge, in stops (centred on the edge).
    pub falloff: f32,
    pub adjust: ZoneAdjust,
}

/// HDR zone wheels (fork G4.2), on working linear light (scene-referred in DWG).
///
/// Per pixel: the luminance `Y = weights · rgb` as stops from the pivot,
/// `s = log2(Y / pivot)` (clamped to ±[`ZONE_STOP_LIMIT`]; `Y` ≤ 0 is the lowest). Each zone's
/// weight is a smoothstep across its soft edge, `u = smoothstep((s − edge) / falloff + ½)`: a dark
/// zone (the first [`ZONE_DARK`]) has weight `1 − u` (everything below its edge), a light zone
/// `u` (everything above). Zones nest and overlap like Resolve's (a deep shadow is in Black, Dark
/// and Shadow); their effects add up in exposure terms:
///
/// - exposure `E = global + Σ wᵢ·eᵢ` stops (clamped to ±[`ZONE_MAX_STOPS`]),
/// - balance `D = global + Σ wᵢ·dᵢ` (luminance-neutral shifts),
/// - saturation `S = global · Π (1 + wᵢ·(satᵢ − 1))`.
///
/// Then `rgb₁ = rgb + Y·D` (keeps the luminance; the same chromaticity shift at every exposure),
/// `rgb₂ = rgb₁ · 2^E`, and `rgb₃ = Y₂ + (rgb₂ − Y₂)·S` with `Y₂ = weights · rgb₂`; clamped to
/// ±[`ZONE_MAX_VALUE`], a NaN result (a NaN or ∞ pixel) becomes 0. The weights are smooth in
/// `s`, so there is no banding at zone edges.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneWheels {
    /// Luminance weights of the working gamut.
    pub weights: [f32; 3],
    /// The luminance of mid grey (the stops' zero), > 0.
    pub pivot: f32,
    pub global: ZoneAdjust,
    /// Black, Dark, Shadow, Light, Highlight, Specular.
    pub zones: [Zone; ZONE_COUNT],
}

impl ZoneWheels {
    /// The weight of zone `i` at `s` stops from the pivot (0 for an index past the zones).
    pub fn weight(&self, i: usize, s: f32) -> f32 {
        let Some(z) = self.zones.get(i) else { return 0.0 };
        zone_weight(i < ZONE_DARK, s, z.edge, z.falloff)
    }

    /// Stops from the pivot of luminance `y` (what the zones see).
    pub fn stops(&self, y: f32) -> f32 {
        let pivot = if self.pivot.is_finite() && self.pivot > 0.0 { self.pivot } else { 0.18 };
        let r = y / pivot;
        // (NaN, 0 and negative: the lowest)
        if r > (-ZONE_STOP_LIMIT).exp2() { r.log2().min(ZONE_STOP_LIMIT) } else { -ZONE_STOP_LIMIT }
    }

    // (max then min instead of `clamp`: NaN never propagates into the gain, as on the GPU)
    #[allow(clippy::manual_clamp)]
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let w = self.weights;
        let y = w[0] * c[0] + w[1] * c[1] + w[2] * c[2];
        let s = self.stops(y);
        let mut e = self.global.exposure;
        let mut sat = self.global.sat;
        let mut d = self.global.shift;
        for (i, z) in self.zones.iter().enumerate() {
            let k = zone_weight(i < ZONE_DARK, s, z.edge, z.falloff);
            e += k * z.adjust.exposure;
            sat *= 1.0 + k * (z.adjust.sat - 1.0);
            for (dc, zc) in d.iter_mut().zip(z.adjust.shift) {
                *dc += k * zc;
            }
        }
        // (max then min: NaN-free, as on the GPU)
        let gain = e.max(-ZONE_MAX_STOPS).min(ZONE_MAX_STOPS).exp2();
        let sat = sat.max(0.0).min(ZONE_MAX_SAT);
        let c2 = [0, 1, 2].map(|k| (c[k] + y * d[k]) * gain);
        let y2 = w[0] * c2[0] + w[1] * c2[1] + w[2] * c2[2];
        c2.map(|v| {
            let o = y2 + (v - y2) * sat;
            // (NaN: 0, the same on the GPU)
            if o.is_nan() { 0.0 } else { o.max(-ZONE_MAX_VALUE).min(ZONE_MAX_VALUE) }
        })
    }

    fn is_finite(&self) -> bool {
        let adj = |a: &ZoneAdjust| a.exposure.is_finite() && a.sat.is_finite() && a.shift.iter().all(|v| v.is_finite());
        self.weights.iter().all(|v| v.is_finite())
            && self.pivot.is_finite()
            && adj(&self.global)
            && self.zones.iter().all(|z| z.edge.is_finite() && z.falloff.is_finite() && adj(&z.adjust))
    }
}

/// A zone's weight at `s` stops: `smoothstep` across `edge ± falloff / 2`, reversed for a `dark`
/// zone.
#[inline]
pub fn zone_weight(dark: bool, s: f32, edge: f32, falloff: f32) -> f32 {
    let f = if falloff.is_finite() { falloff.max(ZONE_MIN_FALLOFF) } else { 1.0 };
    let a = ((s - edge) / f + 0.5).clamp(0.0, 1.0);
    let u = a * a * (3.0 - 2.0 * a);
    if dark { 1.0 - u } else { u }
}

/// Half-range (not peak-to-peak) of the grain at strength 1 in the mid-tones, on the grading
/// signal: hard cells offset by at most ±GRAIN_AMPLITUDE (softened cells are renormalised to the
/// same standard deviation).
pub const GRAIN_AMPLITUDE: f32 = 0.15;

/// Grain left in pure black and white, relative to the mid-tones.
pub const GRAIN_FLOOR: f32 = 0.1;

/// The finest and coarsest grain cells (`1 / inv_size`, pixels) the grain draws.
pub const GRAIN_MIN_SIZE: f32 = 0.25;
pub const GRAIN_MAX_SIZE: f32 = 4096.0;

/// The integer hash of the grain (lowbias32; wrapping, identical in WGSL).
#[inline]
pub fn grain_hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// The random value of grain cell (`ix`, `iy`): the sum of two uniform values − 1, a triangular
/// distribution on −1…1 (computed exactly in `f32`).
#[inline]
fn grain_cell(ix: u32, iy: u32, seed: u32) -> f32 {
    let h = grain_hash(seed ^ grain_hash(iy ^ grain_hash(ix)));
    let a = (h >> 8) as f32 * (1.0 / 16_777_216.0);
    let b = (grain_hash(h) >> 8) as f32 * (1.0 / 16_777_216.0);
    a + b - 1.0
}

/// Grain noise at pixel `xy` (pixel index): cells of `1 / inv_size` pixels, each with a random
/// value; `softness` 0 draws the cells hard, 1 interpolates smoothly (smoothstep) between
/// neighbouring cells. The result is normalised by the interpolation weights so softening keeps
/// the grain's variance.
pub fn grain_noise(xy: [f32; 2], inv_size: f32, softness: f32, seed: u32) -> f32 {
    let inv = if inv_size.is_finite() { inv_size.clamp(1.0 / GRAIN_MAX_SIZE, 1.0 / GRAIN_MIN_SIZE) } else { 1.0 };
    let soft = if softness.is_finite() { softness.clamp(0.0, 1.0) } else { 0.0 };
    let p = xy.map(|v| if v.is_finite() { ((v + 0.5) * inv).clamp(-1e7, 1e7) } else { 0.0 });
    let i = p.map(f32::floor);
    let s = [p[0] - i[0], p[1] - i[1]].map(|t| soft * (t * t * (3.0 - 2.0 * t)));
    // (−1e7…1e7 fits i32; the cell index wraps as u32, as WGSL's bitcast does)
    let (ix, iy) = (i[0] as i32 as u32, i[1] as i32 as u32);
    let (ix1, iy1) = (ix.wrapping_add(1), iy.wrapping_add(1));
    let (wx0, wx1, wy0, wy1) = (1.0 - s[0], s[0], 1.0 - s[1], s[1]);
    let v =
        wy0 * (wx0 * grain_cell(ix, iy, seed) + wx1 * grain_cell(ix1, iy, seed)) + wy1 * (wx0 * grain_cell(ix, iy1, seed) + wx1 * grain_cell(ix1, iy1, seed));
    // (√ of a product of two sums ≥ ½ each: at least ½; the max only guards a division by 0)
    let norm = ((wx0 * wx0 + wx1 * wx1) * (wy0 * wy0 + wy1 * wy1)).sqrt().max(0.25);
    v / norm
}

impl ColorOp {
    /// The identity matrix op with `m` (no offset).
    pub fn matrix(m: Mat3) -> ColorOp {
        ColorOp::Matrix { m: m.map(|r| r.map(|v| v as f32)), offset: [0.0; 3] }
    }

    /// A camera log signal → scene-linear, for a decoder-normalised value in `range` (the
    /// curve's own domain conversion folded in), sampled over the whole signal range.
    pub fn decode_log(curve: LogCurve, range: Range) -> ColorOp {
        let d: Domain = curve.domain();
        // a little beyond 0…1 on both sides: super-black / super-white codes, filter overshoot
        ColorOp::Table(Table::sample(-0.125, 1.125, TABLE_SIZE, |v| curve.decode(signal_from_normalized(v, range, d))))
    }

    /// Scene-linear → a camera log signal (decoder-normalised in `range`): for test pictures and
    /// round trips; sampled in DaVinci Intermediate (a log axis covering 0 … 100).
    pub fn encode_log(curve: LogCurve, range: Range) -> Vec<ColorOp> {
        let d = curve.domain();
        vec![
            ColorOp::Encode(Tf::DaVinciIntermediate),
            ColorOp::Table(Table::sample(-0.05, 1.05, TABLE_SIZE, |y| normalized_from_signal(curve.encode(f64::from(di_decode(y as f32))), range, d))),
        ]
    }

    /// Every number in it is finite (what the GPU is handed).
    pub fn is_finite(&self) -> bool {
        let all = |v: &[f32]| v.iter().all(|x| x.is_finite());
        match self {
            ColorOp::Matrix { m, offset } => m.iter().all(|r| all(r)) && all(offset),
            ColorOp::Decode(_) | ColorOp::Encode(_) => true,
            ColorOp::Table(t) => t.lo.is_finite() && t.hi.is_finite() && all(&t.values),
            ColorOp::Lggo { lift, gamma, gain, offset } => all(lift) && all(gamma) && all(gain) && all(offset),
            ColorOp::Saturation { sat, vibrance, weights } => sat.is_finite() && vibrance.is_finite() && all(weights),
            ColorOp::HlgOotf { weights, k, exponent } => all(weights) && k.is_finite() && exponent.is_finite(),
            ColorOp::ToneMap { src_peak, dst_peak, ratio } => {
                src_peak.is_finite() && dst_peak.is_finite() && ratio.lo.is_finite() && ratio.hi.is_finite() && all(&ratio.values)
            }
            ColorOp::GamutCompress => true,
            ColorOp::DesaturateIntoGamut { weights } => all(weights),
            ColorOp::Clamp { lo, hi } => lo.is_finite() && hi.is_finite(),
            ColorOp::ToneRegions { shadows, highlights, weights } => shadows.is_finite() && highlights.is_finite() && all(weights),
            ColorOp::Contrast { amount, .. } => amount.is_finite(),
            ColorOp::Specular { gain } => gain.is_finite(),
            ColorOp::SplitTone { shadows, highlights, weights } => all(shadows) && all(highlights) && all(weights),
            ColorOp::Vignette { amount, midpoint, feather, sx, size } => all(&[*amount, *midpoint, *feather, *sx]) && all(size),
            ColorOp::Curve { table, .. } => table.lo.is_finite() && table.hi.is_finite() && all(&table.values),
            ColorOp::Wheels { shadows, midtones, highlights, weights } => all(shadows) && all(midtones) && all(highlights) && all(weights),
            ColorOp::HslCurves(h) => h.tables().iter().all(|t| t.as_ref().is_none_or(|t| t.lo.is_finite() && t.hi.is_finite() && all(&t.values))),
            ColorOp::Lut { lut, intensity, .. } => intensity.is_finite() && lut_ok(lut),
            ColorOp::Look { intensity, .. } => intensity.is_finite(),
            ColorOp::HslKey { hue, range, sat_min, sat_max, luma_min, luma_max, soft, .. } => {
                all(&[*hue, *range, *sat_min, *sat_max, *luma_min, *luma_max, *soft])
            }
            ColorOp::HslCorrect { hue_shift, sat, temp, tint, .. } => all(&[*hue_shift, *sat, *temp, *tint]),
            ColorOp::Grain { strength, inv_size, softness, weights, .. } => all(&[*strength, *inv_size, *softness]) && all(weights),
            ColorOp::Stash | ColorOp::MixStash { .. } => true,
            ColorOp::ZoneWheels(z) => z.is_finite(),
            // (finite by construction: `Warp2D::new`)
            ColorOp::Warp2D(w) => w.field().iter().chain(&w.weights()).all(|v| v.is_finite()),
            // (finite by construction: `Key3d::new`)
            ColorOp::Key3d(k) => k.soft().is_finite() && k.include().iter().chain(k.exclude()).flatten().all(|v| v.is_finite()),
            ColorOp::Despill { dir, amount } => all(dir) && amount.is_finite(),
        }
    }

    /// An upper bound of the `f32` values the GPU program of this op takes.
    pub fn gpu_size(&self) -> usize {
        let t = |t: &Table| t.values.len() + 4;
        16 + match self {
            ColorOp::Table(x) | ColorOp::ToneMap { ratio: x, .. } | ColorOp::Curve { table: x, .. } => t(x),
            ColorOp::HslCurves(h) => h.tables().iter().map(|x| x.as_ref().map_or(4, t)).sum(),
            ColorOp::Lut { lut, .. } => lut
                .shaper
                .as_ref()
                .map_or(0, |s| s.data.len().saturating_mul(3))
                .saturating_add(lut.cube.as_ref().map_or(0, |c| c.data.len().saturating_mul(3))),
            ColorOp::ZoneWheels(_) => ZONE_PROGRAM_LEN,
            ColorOp::Warp2D(w) => w.field().len() + 8,
            ColorOp::Key3d(k) => 4 * (k.include().len() + k.exclude().len()) + 8,
            _ => 0,
        }
    }

    /// Apply to one pixel (positional operations see pixel 0, 0: use [`apply_at`](Self::apply_at)).
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        self.apply_at(c, [0.0; 2])
    }

    /// Apply to the pixel at `xy` (pixel indices in the image the chain runs on).
    pub fn apply_at(&self, c: [f32; 3], xy: [f32; 2]) -> [f32; 3] {
        self.apply_px(c, xy, &mut Regs::new(c))
    }

    /// Apply to the pixel at `xy` with the registers `regs` (keys write the mask, masked
    /// corrections read it; [`ColorOp::Stash`] / [`ColorOp::MixStash`] use the stash).
    pub fn apply_px(&self, c: [f32; 3], xy: [f32; 2], regs: &mut Regs) -> [f32; 3] {
        match self {
            ColorOp::HslKey { hue, range, sat_min, sat_max, luma_min, luma_max, soft, hdr } => {
                let u = to_unit(c, *hdr);
                let h = crate::rgb_to_hsl(u[0], u[1], u[2]);
                let dh = (h[0] - hue).abs().min(1.0 - (h[0] - hue).abs());
                let mh = 1.0 - ((dh - range / 2.0) / soft).clamp(0.0, 1.0);
                let mut ms = ((h[1] - sat_min) / soft).clamp(0.0, 1.0);
                if *sat_max < 1.0 {
                    ms = ms.min(((sat_max - h[1]) / soft).clamp(0.0, 1.0));
                }
                let ml = ((h[2] - luma_min) / soft).clamp(0.0, 1.0).min(((luma_max - h[2]) / soft).clamp(0.0, 1.0));
                regs.mask = mh * ms * ml;
                c
            }
            ColorOp::Key3d(k) => {
                regs.mask = k.matte(c);
                c
            }
            ColorOp::Stash => {
                regs.stash = c;
                c
            }
            ColorOp::MixStash { invert } => {
                let m = if *invert { 1.0 - regs.mask } else { regs.mask };
                regs.mask = m;
                let s = regs.stash;
                [0, 1, 2].map(|k| s[k] + (c[k] - s[k]) * m)
            }
            ColorOp::HslCorrect { hue_shift, sat, temp, tint, show_mask, hdr } => {
                let m = regs.mask;
                let u = to_unit(c, *hdr);
                let scale = if *hdr { c[0].max(c[1]).max(c[2]).max(1.0) } else { 1.0 };
                let out = match show_mask {
                    1 => {
                        let g = crate::rgb_to_hsl(u[0], u[1], u[2])[2];
                        u.map(|q| g + (q - g) * m)
                    }
                    2 => u.map(|q| q * m),
                    3 => [m; 3],
                    _ => {
                        let mut hh = crate::rgb_to_hsl(u[0], u[1], u[2]);
                        hh[0] = (hh[0] + hue_shift).rem_euclid(1.0);
                        hh[1] = (hh[1] * sat).clamp(0.0, 1.0);
                        let mut c2 = crate::hsl_to_rgb(hh[0], hh[1], hh[2]);
                        c2[0] *= 1.0 + 0.25 * temp;
                        c2[2] *= 1.0 - 0.25 * temp;
                        c2[1] *= 1.0 - 0.2 * tint;
                        [0, 1, 2].map(|k| u[k] + (c2[k] - u[k]) * m)
                    }
                };
                // the mask views are display images (no HDR scaling)
                if *show_mask == 0 { out.map(|q| q * scale) } else { out }
            }
            _ => self.apply_plain(c, xy),
        }
    }

    fn apply_plain(&self, c: [f32; 3], xy: [f32; 2]) -> [f32; 3] {
        match self {
            ColorOp::Matrix { m, offset } => crate::spaces::apply3(m, c).map_with(offset, |v, o| v + o),
            ColorOp::Decode(t) => c.map(|v| t.decode(v)),
            ColorOp::Encode(t) => c.map(|v| t.encode(v)),
            ColorOp::Table(t) => c.map(|v| t.eval(v)),
            ColorOp::Lggo { lift, gamma, gain, offset } => {
                let mut o = [0.0; 3];
                for (k, out) in o.iter_mut().enumerate() {
                    let (l, g, n, of) = (lift[k], gamma[k], gain[k], offset[k]);
                    let a = l + (n - l) * c[k];
                    let p = if g > 1e-6 { a.abs().powf(1.0 / g).copysign(a) } else { a };
                    *out = p + of;
                }
                o
            }
            ColorOp::Saturation { sat, vibrance, weights } => {
                let l = weights[0] * c[0] + weights[1] * c[1] + weights[2] * c[2];
                let spread = c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
                let s = sat * (1.0 + vibrance * (1.0 - spread.clamp(0.0, 1.0)));
                c.map(|v| l + (v - l) * s)
            }
            ColorOp::HlgOotf { weights, k, exponent } => {
                let ys = weights[0] * c[0] + weights[1] * c[1] + weights[2] * c[2];
                // (no light: black, also for the negative exponent of the inverse OOTF)
                if ys.is_nan() || ys <= 0.0 {
                    return [0.0; 3];
                }
                let g = k * ys.powf(*exponent);
                c.map(|v| v * g)
            }
            ColorOp::ToneMap { src_peak, dst_peak, ratio } => {
                let m = c[0].max(c[1]).max(c[2]);
                if m <= 0.0 {
                    return c;
                }
                let white = crate::transform::REFERENCE_WHITE_NITS as f32;
                let nits = m * white;
                let k = if nits >= *src_peak { dst_peak / white / m } else { ratio.eval(pq_inverse_eotf(nits / 10_000.0)) };
                c.map(|v| v * k)
            }
            ColorOp::GamutCompress => crate::transform::gamut_compress(c),
            ColorOp::DesaturateIntoGamut { weights } => crate::transform::desaturate_into_gamut(c, *weights),
            // (max then min: NaN-free like WGSL's clamp for lo ≤ hi)
            ColorOp::Clamp { lo, hi } => c.map(|v| v.max(*lo).min(*hi)),
            ColorOp::ToneRegions { shadows, highlights, weights } => {
                let l = weights[0] * c[0] + weights[1] * c[1] + weights[2] * c[2];
                let s = (1.0 - l).clamp(0.0, 1.0);
                let h = l.clamp(0.0, 1.0);
                let nl = (l + shadows * (s * s * s) + highlights * (h * h * h)).max(0.0);
                if l > 1e-5 { c.map(|v| v * (nl / l)) } else { c }
            }
            ColorOp::Contrast { amount, keep_above_one } => {
                let k = 1.0 + amount;
                c.map(|q| {
                    if *keep_above_one && q > 1.0 {
                        return q;
                    }
                    let q = q.clamp(0.0, 1.0);
                    let s = q * q * (3.0 - 2.0 * q);
                    if k >= 1.0 { q + (s - q) * (k - 1.0) } else { 0.5 + (q - 0.5) * k }
                })
            }
            ColorOp::Specular { gain } => c.map(|q| if q > 1.0 { 1.0 + (q - 1.0) * gain } else { q }),
            ColorOp::SplitTone { shadows, highlights, weights } => {
                let l = (weights[0] * c[0] + weights[1] * c[1] + weights[2] * c[2]).clamp(0.0, 1.0);
                [0, 1, 2].map(|k| c[k] + (shadows[k] * (1.0 - l) + highlights[k] * l))
            }
            ColorOp::Vignette { amount, midpoint, feather, sx, size } => {
                let nx = (xy[0] / size[0] - 0.5) * 2.0 * sx;
                let ny = (xy[1] / size[1] - 0.5) * 2.0;
                let d = (nx * nx + ny * ny).sqrt() / std::f32::consts::SQRT_2;
                let edge = ((d - midpoint) / feather.max(1e-6)).clamp(0.0, 1.0);
                let e2 = edge * edge * (3.0 - 2.0 * edge);
                let k = 1.0 + amount * 0.2 * e2;
                if *amount < 0.0 { c.map(|q| q * k.max(0.0)) } else { c.map(|q| q + (1.0 - q) * (k - 1.0)) }
            }
            ColorOp::Curve { table, channels, hdr } => [0, 1, 2].map(|k| if channels[k] { table.eval_curve(c[k], *hdr) } else { c[k] }),
            ColorOp::Wheels { shadows, midtones, highlights, weights } => {
                let l = (weights[0] * c[0] + weights[1] * c[1] + weights[2] * c[2]).clamp(0.0, 1.0);
                let wsh = (1.0 - l) * (1.0 - l);
                let whi = l * l;
                let wmid = (1.0 - wsh - whi).max(0.0);
                [0, 1, 2].map(|k| {
                    let v = c[k] + shadows[k] * wsh;
                    let v = v + midtones[k] * wmid;
                    v * (1.0 + highlights[k] * whi)
                })
            }
            ColorOp::HslCurves(h) => h.apply(c),
            ColorOp::ZoneWheels(z) => z.apply(c),
            ColorOp::Warp2D(w) => w.apply(c),
            ColorOp::Despill { dir, amount } => crate::key3d::despill(c, *dir, *amount),
            ColorOp::Lut { lut, intensity, hdr } => {
                if !lut_ok(lut) {
                    return c;
                }
                let lk = within_unit(c, *hdr, |u| lut.apply(u));
                [0, 1, 2].map(|k| c[k] + (lk[k] - c[k]) * intensity)
            }
            ColorOp::Look { look, intensity, hdr } => {
                let lk = if *hdr { within_unit(c, true, |u| apply_look(*look, u)) } else { apply_look(*look, c) };
                [0, 1, 2].map(|k| c[k] + (lk[k] - c[k]) * intensity)
            }
            ColorOp::Grain { strength, inv_size, softness, seed, weights } => {
                let l = (weights[0] * c[0] + weights[1] * c[1] + weights[2] * c[2]).clamp(0.0, 1.0);
                let wl = GRAIN_FLOOR + (1.0 - GRAIN_FLOOR) * 4.0 * l * (1.0 - l);
                let d = strength * GRAIN_AMPLITUDE * wl * grain_noise(xy, *inv_size, *softness, *seed);
                c.map(|v| v + d)
            }
            // (register operations: `apply_px`)
            ColorOp::HslKey { .. } | ColorOp::Key3d(_) | ColorOp::HslCorrect { .. } | ColorOp::Stash | ColorOp::MixStash { .. } => {
                self.apply_px(c, xy, &mut Regs::new(c))
            }
        }
    }
}

/// The GPU program size of a chain ([`MAX_GPU_PROGRAM`] at most to run on the GPU).
pub fn gpu_size(ops: &[ColorOp]) -> usize {
    ops.iter().fold(0usize, |a, o| a.saturating_add(o.gpu_size()))
}

/// Run a chain of operations on one pixel (positional operations see pixel 0, 0).
pub fn apply_chain(ops: &[ColorOp], c: [f32; 3]) -> [f32; 3] {
    apply_chain_at(ops, c, [0.0; 2])
}

/// Run a chain of operations on the pixel at `xy`.
pub fn apply_chain_at(ops: &[ColorOp], c: [f32; 3], xy: [f32; 2]) -> [f32; 3] {
    apply_chain_px(ops, c, xy, 1.0).0
}

/// Run a chain of operations on the pixel at `xy` starting with the mask register at `mask`:
/// the colour and the mask afterwards.
pub fn apply_chain_px(ops: &[ColorOp], mut c: [f32; 3], xy: [f32; 2], mask: f32) -> ([f32; 3], f32) {
    let mut regs = Regs { mask, stash: c };
    for op in ops {
        c = op.apply_px(c, xy, &mut regs);
    }
    (c, regs.mask)
}

/// A pixel's registers while a chain runs: the mask (keys write it, masked corrections read it;
/// 1 when nothing set it) and the stash ([`ColorOp::Stash`]; the chain's input until then).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Regs {
    pub mask: f32,
    pub stash: [f32; 3],
}

impl Regs {
    /// Fresh registers for a chain whose input is `c`.
    pub fn new(c: [f32; 3]) -> Self {
        Self { mask: 1.0, stash: c }
    }
}

/// A gamut conversion (colour space transform of linear light).
pub fn gamut_op(src: Gamut, dst: Gamut) -> ColorOp {
    ColorOp::matrix(crate::spaces::gamut_matrix(src, dst))
}

trait MapWith {
    fn map_with(self, o: &[f32; 3], f: impl Fn(f32, f32) -> f32) -> [f32; 3];
}

impl MapWith for [f32; 3] {
    fn map_with(self, o: &[f32; 3], f: impl Fn(f32, f32) -> f32) -> [f32; 3] {
        [f(self[0], o[0]), f(self[1], o[1]), f(self[2], o[2])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, rel: f32) -> bool {
        (a - b).abs() <= rel * b.abs().max(1e-3)
    }

    #[test]
    fn davinci_intermediate_published_points_and_round_trip() {
        // the two halves meet at the published cuts
        assert!((di_encode(di::LIN_CUT) - di::LOG_CUT).abs() < 1e-6);
        assert!(((di::LIN_CUT + di::A).log2() + di::B) * di::C - di::LOG_CUT < 1e-6);
        // 18 % grey → 0.336, 1.0 → 0.514, 100 → 1.000 (three places)
        for (x, y) in [(0.18f32, 0.336f32), (1.0, 0.514), (100.0, 1.0)] {
            assert!((di_encode(x) - y).abs() < 6e-4, "{x}: {} vs {y}", di_encode(x));
        }
        for i in 0..=2000 {
            let x = -0.01 + (i as f32 / 2000.0).powi(3) * 100.0;
            assert!(close(di_decode(di_encode(x)), x, 2e-5), "{x}");
        }
    }

    #[test]
    fn transfer_functions_round_trip() {
        for t in [Tf::Srgb, Tf::Gamma24, Tf::Pq, Tf::Hlg, Tf::DaVinciIntermediate] {
            for i in 0..=200 {
                let x = i as f32 / 200.0;
                assert!(close(t.decode(t.encode(x)), x, 2e-4), "{t:?} {x}");
            }
        }
    }

    #[test]
    fn sampled_log_decoding_matches_the_curves() {
        for curve in LogCurve::ALL {
            for range in [Range::Limited, Range::Full] {
                let op = ColorOp::decode_log(curve, range);
                for i in 0..=1000 {
                    let v = i as f64 / 1000.0;
                    let want = curve.decode(signal_from_normalized(v, range, curve.domain())) as f32;
                    let got = op.apply([v as f32; 3])[0];
                    // A linear table cannot follow a curve's kink exactly: S-Log3's segments, F-Log's
                    // rounded halves (1e-4 apart) and Apple Log's square root at black are off by
                    // ~1e-5 of linear light next to them (Apple Log ~1e-3: a fifth of a 10-bit code).
                    let abs = if curve == LogCurve::AppleLog { 1e-3 } else { 1.5e-5 };
                    assert!((got - want).abs() <= 2e-5 * want.abs() + abs, "{curve:?} {range:?} {v}: {got} vs {want}");
                }
            }
        }
    }

    #[test]
    fn f_log2_grey_through_a_davinci_working_space_and_back() {
        // F-Log2 / F-Gamut code 400 (18 % grey) → linear DWG → DaVinci Intermediate 0.336
        let to_dwg = gamut_op(Gamut::Bt2020, Gamut::DaVinciWide);
        let chain = [ColorOp::decode_log(LogCurve::FLog2, Range::Full), to_dwg, ColorOp::Encode(Tf::DaVinciIntermediate)];
        let di = apply_chain(&chain, [400.0 / 1023.0; 3]);
        assert!(di.iter().all(|v| (v - 0.336).abs() < 1e-3), "{di:?}");
        let back = apply_chain(&ColorOp::encode_log(LogCurve::FLog2, Range::Full), [0.18; 3]);
        assert!(back.iter().all(|v| (v * 1023.0 - 400.0).abs() < 0.5), "{back:?}");
    }

    #[test]
    fn tables_interpolate_and_extend() {
        let t = Table::sample(0.0, 1.0, 3, |x| 2.0 * x);
        assert_eq!([t.eval(0.25), t.eval(1.5), t.eval(-0.5)], [0.5, 3.0, -1.0]);
        let empty = Table { lo: 0.0, hi: 1.0, values: Vec::new().into() };
        assert_eq!(empty.eval(0.3), 0.3);
        let flat = Table { lo: 1.0, hi: 1.0, values: vec![2.0, 3.0].into() };
        assert_eq!(flat.eval(5.0), 2.0);
    }

    #[test]
    fn lggo_and_saturation() {
        let id = ColorOp::Lggo { lift: [0.0; 3], gamma: [1.0; 3], gain: [1.0; 3], offset: [0.0; 3] };
        assert_eq!(id.apply([0.2, 0.5, 0.9]), [0.2, 0.5, 0.9]);
        let g = ColorOp::Lggo { lift: [0.1, 0.0, 0.0], gamma: [1.0, 2.0, 1.0], gain: [1.0, 1.0, 0.5], offset: [0.0, 0.0, 0.1] };
        let o = g.apply([0.0, 0.25, 1.0]);
        assert!((o[0] - 0.1).abs() < 1e-6 && (o[1] - 0.5).abs() < 1e-6 && (o[2] - 0.6).abs() < 1e-6, "{o:?}");
        let s = ColorOp::Saturation { sat: 0.0, vibrance: 0.0, weights: [0.2126, 0.7152, 0.0722] };
        let grey = s.apply([1.0, 0.0, 0.0]);
        assert!(grey.iter().all(|v| (v - 0.2126).abs() < 1e-6));
    }

    fn grain(strength: f32, size: f32, softness: f32, seed: u32) -> ColorOp {
        ColorOp::Grain { strength, inv_size: 1.0 / size, softness, seed, weights: [0.2126, 0.7152, 0.0722] }
    }

    /// Mean absolute grain offset over a 64×64 patch of `level`.
    fn grain_mad(op: &ColorOp, level: f32) -> f32 {
        let mut sum = 0.0;
        for y in 0..64 {
            for x in 0..64 {
                sum += (op.apply_at([level; 3], [x as f32, y as f32])[0] - level).abs();
            }
        }
        sum / 4096.0
    }

    #[test]
    fn grain_is_deterministic_per_seed_and_monochrome() {
        let a = grain(0.8, 1.5, 0.5, 42);
        let b = grain(0.8, 1.5, 0.5, 43);
        let mut differs = 0;
        for y in 0..32 {
            for x in 0..32 {
                let xy = [x as f32, y as f32];
                let (p, q) = (a.apply_at([0.4, 0.3, 0.2], xy), a.apply_at([0.4, 0.3, 0.2], xy));
                assert_eq!(p, q);
                // the same offset on every channel (monochrome grain on the luma)
                assert!(((p[0] - 0.4) - (p[1] - 0.3)).abs() < 1e-6 && ((p[1] - 0.3) - (p[2] - 0.2)).abs() < 1e-6);
                differs += usize::from(p != b.apply_at([0.4, 0.3, 0.2], xy));
            }
        }
        assert!(differs > 1000, "{differs}");
    }

    #[test]
    fn grain_is_strongest_in_the_mid_tones() {
        let g = grain(1.0, 1.0, 0.0, 7);
        let (black, mid, white) = (grain_mad(&g, 0.0), grain_mad(&g, 0.5), grain_mad(&g, 1.0));
        assert!(mid > 0.03, "{mid}");
        assert!(black < mid * 0.2 && white < mid * 0.2, "{black} {mid} {white}");
        // softening keeps the grain's strength roughly (normalised interpolation)
        let soft = grain_mad(&grain(1.0, 1.0, 1.0, 7), 0.5);
        assert!(soft > mid * 0.6 && soft < mid * 1.4, "{soft} vs {mid}");
        // strength 0 is the identity
        assert_eq!(grain(0.0, 2.0, 0.5, 1).apply_at([0.3, 0.5, 0.7], [3.0, 9.0]), [0.3, 0.5, 0.7]);
    }

    #[test]
    fn grain_size_sets_the_cell_size() {
        // hard 4 px cells: the 4×4 pixels of a cell share one value
        let g = grain(1.0, 4.0, 0.0, 99);
        let at = |x: f32, y: f32| g.apply_at([0.5; 3], [x, y])[0];
        assert_eq!(at(0.0, 0.0), at(2.0, 2.0));
        assert_ne!(at(0.0, 0.0), at(4.0, 0.0));
    }

    #[test]
    fn hostile_grain_never_panics_or_poisons_finite_pixels() {
        for (inv, soft) in [(f32::NAN, f32::NAN), (f32::INFINITY, -1.0), (0.0, 2.0), (-5.0, 0.5), (1e30, 1e30)] {
            let op = ColorOp::Grain { strength: 1.0, inv_size: inv, softness: soft, seed: u32::MAX, weights: [0.3; 3] };
            for xy in [[0.0, 0.0], [f32::MAX, -f32::MAX], [f32::NAN, f32::INFINITY], [1e9, 1e9]] {
                let o = op.apply_at([0.5; 3], xy);
                assert!(o.iter().all(|v| v.is_finite()), "{inv} {soft} {xy:?}: {o:?}");
            }
        }
        assert!(!ColorOp::Grain { strength: f32::NAN, inv_size: 1.0, softness: 0.5, seed: 0, weights: [0.3; 3] }.is_finite());
    }

    /// Zones at the defaults of `filmcraft_project::grade` (edges −4, −2.5, −1, 1, 2.5, 4.5;
    /// falloff 1), nothing set.
    fn zones() -> ZoneWheels {
        let z = |edge| Zone { edge, falloff: 1.0, adjust: ZoneAdjust::IDENTITY };
        ZoneWheels { weights: [0.25, 0.5, 0.25], pivot: 0.18, global: ZoneAdjust::IDENTITY, zones: [-4.0, -2.5, -1.0, 1.0, 2.5, 4.5].map(z) }
    }

    /// Fork G4.2: two hand-computed pixels. Grey 0.36 is 1 stop over the pivot: half in Light
    /// (on its edge), in no other zone. Grey 0.09 is 1 stop under: half in Shadow.
    #[test]
    fn zone_wheels_match_hand_computed_pixels() {
        let mut z = zones();
        z.global.exposure = -1.0;
        z.zones[3].adjust = ZoneAdjust { exposure: 2.0, sat: 0.0, shift: [0.2, -0.1, 0.0] };
        z.zones[2].adjust.exposure = 1.0;
        assert_eq!([0, 1, 2, 3, 4, 5].map(|i| z.weight(i, 1.0)), [0.0, 0.0, 0.0, 0.5, 0.0, 0.0]);
        // E = −1 + ½·2 = 0; D = ½·(0.2, −0.1, 0); rgb₁ = 0.36·(1 + D) = (0.396, 0.342, 0.36), whose
        // luminance stays 0.36; S = 1 + ½·(0 − 1) = ½
        let o = z.apply([0.36; 3]);
        for (got, want) in o.iter().zip([0.378f32, 0.351, 0.36]) {
            assert!((got - want).abs() < 1e-6, "{o:?}");
        }
        // E = −1 + ½·1 = −½ stop
        let o = z.apply([0.09; 3]);
        assert!(o.iter().all(|v| (v - 0.09 * 0.5f32.sqrt()).abs() < 1e-6), "{o:?}");
        // the stops: 0.18 is 0, 0 and negatives the lowest, NaN too
        assert_eq!([z.stops(0.18), z.stops(0.72), z.stops(0.0), z.stops(-1.0), z.stops(f32::NAN)], [0.0, 2.0, -30.0, -30.0, -30.0]);
    }

    /// Fork G4.2: the weights are smooth (no banding: no step bigger than the slope of the
    /// narrowest edge allows), dark zones nest below and light zones above, and nothing set is
    /// the identity.
    #[test]
    fn zone_weights_are_smooth_and_nest() {
        let z = zones();
        for i in 0..ZONE_COUNT {
            let mut prev = z.weight(i, -12.0);
            for k in 1..=2400 {
                let s = -12.0 + k as f32 * 0.01;
                let w = z.weight(i, s);
                assert!((w - prev).abs() <= 0.016, "zone {i} jumps at {s}: {prev} → {w}");
                prev = w;
            }
        }
        // a deep shadow is in every dark zone, a bright highlight in Light and Highlight
        assert_eq!([0, 1, 2].map(|i| z.weight(i, -6.0)), [1.0; 3]);
        assert_eq!([3, 4, 5].map(|i| z.weight(i, 3.5)), [1.0, 1.0, 0.0]);
        assert_eq!(z.weight(9, 0.0), 0.0);
        for c in [[0.0f32; 3], [0.18; 3], [0.3, 0.2, 0.1], [50.0, 2.0, -0.1], [-0.2; 3]] {
            let o = z.apply(c);
            assert!(o.iter().zip(c).all(|(a, b)| (a - b).abs() <= 1e-6 * b.abs().max(1.0)), "{c:?} → {o:?}");
        }
    }

    /// Fork G4.2: 0, negative, huge and non-finite pixels and absurd settings never give ∞ (or a
    /// panic); a non-finite op is refused for the GPU.
    #[test]
    fn hostile_zone_wheels_stay_finite() {
        let mut z = zones();
        z.global = ZoneAdjust { exposure: 1e30, sat: 1e30, shift: [1e30, -1e30, 0.0] };
        for zone in &mut z.zones {
            zone.falloff = 0.0;
            zone.adjust = ZoneAdjust { exposure: -1e30, sat: 1e30, shift: [3.0, 0.0, -3.0] };
        }
        z.pivot = -1.0;
        for c in [[0.0f32; 3], [-1.0; 3], [f32::MAX; 3], [1e20, -1e20, 0.0], [f32::INFINITY, 0.0, 0.0], [f32::NAN, 0.2, 0.2]] {
            let o = ColorOp::ZoneWheels(Arc::new(z.clone())).apply(c);
            assert!(o.iter().all(|v| !v.is_infinite()), "{c:?} → {o:?}");
        }
        // a NaN or ∞ pixel comes out as 0 where its result is NaN, never ±1e30 from NaN
        let nan = ColorOp::ZoneWheels(Arc::new(zones())).apply([f32::NAN, 0.2, 0.2]);
        assert_eq!(nan, [0.0; 3]);
        let inf = ColorOp::ZoneWheels(Arc::new(zones())).apply([f32::INFINITY, 0.2, 0.2]);
        assert!(inf.iter().all(|v| v.is_finite()), "{inf:?}");
        let finite = [0.3f32, 0.2, 0.1];
        assert!(ColorOp::ZoneWheels(Arc::new(z.clone())).apply(finite).iter().all(|v| v.is_finite()));
        assert!(ColorOp::ZoneWheels(Arc::new(z.clone())).is_finite());
        z.zones[4].edge = f32::NAN;
        assert!(!ColorOp::ZoneWheels(Arc::new(z)).is_finite());
    }
}
