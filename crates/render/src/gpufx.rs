//! Standard effects with a GPU implementation (`filmcraft_gpu` runs them in WGSL).
//!
//! Each effect here is evaluated in two steps: [`FxOp::eval`] reads the effect's parameters at a
//! time (keyframes, defaults, `px_scale`, the working image size) into plain numbers, and
//! [`FxOp::apply`] runs the CPU reference on a working [`Image`]. `effects::apply` uses exactly
//! these two steps, so the CPU render and the GPU plan agree on every parameter (and on how
//! hostile values are clamped) by construction. The GPU compositor receives the evaluated
//! [`FxOp`]s in a [`crate::plan::LayerFx`] and reproduces `apply` per pixel; the plan only hands an
//! op to the GPU when [`FxOp::gpu_ok`] says the shader covers it (finite numbers, no minifying
//! resample), otherwise the clip is rendered on the CPU as before.

use filmcraft_color::{hsl_to_rgb, linear_to_srgb, luma709, rgb_to_hsl};
use filmcraft_geom::Affine;
use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use crate::effects::{FxCtx, b, choice, color, dec, enc, f, gaussian_boxes, point};
use crate::image::Image;

/// A bilinear resample of the working image through an affine map (`Image::transformed` at a
/// magnification or mild minification: no mip pre-filter).
#[derive(Clone, Debug, PartialEq)]
pub struct Resample {
    /// Destination pixel → source pixel (None: singular map, the result is transparent).
    pub inv: Option<Affine>,
    /// Destination pixels written: x0..x1, y0..y1 (the rest becomes transparent).
    pub rect: [u32; 4],
    /// Alpha scale applied afterwards (Transform's Opacity; 1 = none).
    pub opacity: f32,
}

/// Ultra Key's evaluated numbers (`vfx::color::ultra_key_eval`; the CPU reference is
/// `vfx::color::ultra_key_apply`, the GPU's `fx.wgsl` `OP_UKEY*`).
#[derive(Clone, Debug, PartialEq)]
pub struct UltraKey {
    /// The key colour as sRGB-encoded Rec.709 Y′CbCr.
    pub k: [f32; 3],
    /// |key chroma|², at least 1e-6.
    pub kmag2: f32,
    pub tol: f32,
    pub gain: f32,
    pub highlight: f32,
    pub shadow: f32,
    pub pedestal: f32,
    /// Choke (0..1) and the box radii of its matte blur (empty: no blur).
    pub choke: f32,
    pub choke_rx: Vec<u32>,
    pub choke_ry: Vec<u32>,
    /// Soften: box radii of the matte blur (empty: none).
    pub soften_rx: Vec<u32>,
    pub soften_ry: Vec<u32>,
    pub contrast: f32,
    pub mid: f32,
    pub spill: f32,
    pub desat: f32,
    pub range: f32,
    pub sluma: f32,
    /// Colour correction: saturation, hue (turns), luminance; None: identity.
    pub cc: Option<[f32; 3]>,
    /// The key colour's dominant channel (0 R, 1 G, 2 B).
    pub dom: u32,
    /// 0 Composite, 1 Alpha Channel (the matte), 2 Color Channel.
    pub output: u32,
}

impl UltraKey {
    fn is_finite(&self) -> bool {
        let f = [
            self.kmag2,
            self.tol,
            self.gain,
            self.highlight,
            self.shadow,
            self.pedestal,
            self.choke,
            self.contrast,
            self.mid,
            self.spill,
            self.desat,
            self.range,
            self.sluma,
        ];
        self.k.iter().chain(&f).chain(self.cc.iter().flatten()).all(|v| v.is_finite())
    }
}

/// One evaluated effect.
#[derive(Clone, Debug, PartialEq)]
pub enum FxOp {
    BrightnessContrast {
        br: f32,
        co: f32,
    },
    ProcAmp {
        br: f32,
        co: f32,
        hue: f32,
        sat: f32,
    },
    Tint {
        black: [f32; 3],
        white: [f32; 3],
        amount: f32,
    },
    BlackWhite,
    ColorBalance {
        sh: [f32; 3],
        md: [f32; 3],
        hi: [f32; 3],
        preserve: bool,
    },
    LeaveColor {
        amount: f32,
        key_hue: f32,
        tol: f32,
        soft: f32,
    },
    ChangeToColor {
        from_hue: f32,
        to_hue: f32,
        tol: f32,
        soft: f32,
    },
    ColorPass {
        key: [f32; 3],
        sim: f32,
        reverse: bool,
    },
    Gamma {
        g: f32,
    },
    Levels {
        ib: f32,
        iw: f32,
        ob: f32,
        ow: f32,
        g: f32,
    },
    Extract {
        lo: f32,
        hi: f32,
        soft: f32,
        invert: bool,
    },
    Invert {
        channel: u32,
        blend: f32,
    },
    Posterize {
        n: f32,
    },
    /// Three box passes per axis approximating a Gaussian (radii per pass; empty: axis untouched).
    Gaussian {
        rx: Vec<u32>,
        ry: Vec<u32>,
        repeat: bool,
    },
    /// `steps` bilinear (edge-clamped) taps along `(dx, dy)` centred on the pixel.
    DirectionalBlur {
        dx: f32,
        dy: f32,
        steps: u32,
    },
    /// Unsharp mask over a repeat-edge Gaussian of the given box radii (per axis).
    Unsharp {
        rx: Vec<u32>,
        ry: Vec<u32>,
        amount: f32,
        threshold: f32,
    },
    /// Crop / Edge Feather: alpha falloff `feather` px inside the rectangle (0: half-pixel AA).
    Crop {
        x0: f32,
        x1: f32,
        y0: f32,
        y1: f32,
        feather: f32,
    },
    Resample(Resample),
    HFlip,
    VFlip,
    Mirror {
        cx: f32,
        cy: f32,
        nx: f32,
        ny: f32,
    },
    /// Wrap-around shift (dx, dy already reduced into 0..w, 0..h), mixed with the original.
    Offset {
        dx: f32,
        dy: f32,
        blend: f32,
    },
    /// ASC CDL on display-encoded colour (slope, offset, power, saturation).
    AscCdl {
        slope: [f32; 3],
        offset: [f32; 3],
        power: [f32; 3],
        sat: f32,
    },
    /// Channel Mixer: rows (r, g, b, constant) per output channel, on display-encoded colour.
    ChannelMix {
        m: [[f32; 4]; 3],
    },
    /// Color Replace: `sim` already ×1.2; `replace_hsl` the HSL of the replacement colour.
    ColorReplace {
        sim: f32,
        solid: bool,
        target: [f32; 3],
        replace: [f32; 3],
        replace_hsl: [f32; 3],
    },
    AlphaAdjust {
        opacity: f32,
        ignore: bool,
        invert: bool,
        mask_only: bool,
    },
    /// Ultra Key (fork Y3.1b; boxed: it is the largest op).
    UltraKey(Box<UltraKey>),
    /// Color Key: alpha ramps from 0 at `tol` to 1 at `tol + soft` of a pixel's Y′CbCr distance
    /// from `k` (the key colour's sRGB-encoded Rec.709 Y′CbCr; luma weighs 0.15) (fork Y3.1c).
    ColorKey {
        k: [f32; 3],
        tol: f32,
        soft: f32,
    },
    /// Luma Key: alpha 0 at or below `cut`, 1 at or above `th` (both 0..1 on sRGB-encoded
    /// Rec.709 luma), a linear ramp between; scales the premultiplied pixel (fork Y3.1).
    LumaKey {
        th: f32,
        cut: f32,
    },
    /// A chain of per-pixel colour operations (fork ADR 0003: input transforms, Lumetri), one
    /// pass on the GPU. Built by the renderer, not from a single effect's parameters.
    ColorChain(std::sync::Arc<[filmcraft_color::ops::ColorOp]>),
    /// A keyed correction (Lumetri HSL Secondary): `key` sets each pixel's mask (0 where
    /// transparent), a `median_r` median filter mixed in by `denoise` and box passes of radii
    /// `blur` (rows, then columns; edges clamped) refine it, then `correct` runs with that mask.
    HslSecondary {
        key: std::sync::Arc<[filmcraft_color::ops::ColorOp]>,
        median_r: u32,
        denoise: f32,
        blur: Vec<u32>,
        correct: std::sync::Arc<[filmcraft_color::ops::ColorOp]>,
    },
    /// An effect limited to its masks (`mask::apply_effect`): `ops` run, then the result is mixed
    /// with the image before them by the masks' coverage.
    Masked {
        ops: Vec<FxOp>,
        masks: std::sync::Arc<[crate::mask::FlatMask]>,
    },
    /// An effect limited to a bitmap matte (fork G3.2, Magic Mask): `ops` run, then the result is
    /// mixed with the image before them by the matte's coverage (`matte::coverage`: the matte
    /// stretched bilinearly over the whole image). An invalid matte covers nothing (identity).
    MaskedBy {
        ops: Vec<FxOp>,
        matte: std::sync::Arc<crate::matte::BitmapMatte>,
    },
    /// The clip's opacity masks (`mask::apply_opacity_masks`): pixels scaled by the coverage.
    OpacityMask {
        masks: std::sync::Arc<[crate::mask::FlatMask]>,
    },
    Vignette {
        amount: f32,
        midpoint: f32,
        roundness: f32,
        feather: f32,
        target: [f32; 3],
    },
    VideoLimiter {
        max: f32,
        comp: f32,
        axis: u32,
        warn: bool,
        warning_color: [f32; 3],
    },
    /// Spatial noise reduction (`crate::denoise`): an edge-preserving bilateral filter on a
    /// luma / two-chroma-difference split of the sRGB-encoded straight colour; alpha unchanged.
    /// Every value is sanitised by `denoise::params` (CPU and GPU alike).
    Denoise {
        /// Luma strength 0…1 (0: luma untouched; NaN → 0, clamped). Range sigma = 0.05 × strength.
        luma: f32,
        /// Chroma strength 0…1 (0: chroma untouched; NaN → 0, clamped).
        chroma: f32,
        /// Spatial sigma in working-image pixels (scale it by `px_scale`), clamped to 0.25…3.5;
        /// the window radius is `ceil(2σ)`, 1…=[`crate::denoise::DENOISE_MAX_RADIUS`].
        sigma: f32,
    },
    /// Temporal noise reduction (`crate::tnr`, fork 6.7): motion-compensated blending with up to
    /// `radius` frames each side (`inputs`: the neighbouring frames and their flows, attached by
    /// the renderer). Identity when `inputs` is None or has no usable neighbour, when both
    /// strengths are 0 (or NaN / negative), or at radius 0. Every value is sanitised by
    /// `tnr::params` / `tnr::radius` (CPU and GPU alike).
    TemporalDenoise {
        /// Luma strength 0…1 (range sigma 0.05 × strength in encoded units).
        luma: f32,
        /// Chroma strength 0…1.
        chroma: f32,
        /// Frames each side, 0…=[`crate::tnr::MAX_RADIUS`] (larger: the maximum).
        radius: u8,
        /// Motion tolerance 0…1 (forward–backward disagreement accepted; NaN → 0.5).
        motion: f32,
        inputs: Option<std::sync::Arc<TemporalInputs>>,
    },
    /// A user WGSL shader effect (fork Y3.4, ADR 0006): runs on the GPU only; the CPU leaves the
    /// image as it is.
    User(Box<UserOp>),
    /// A branched colour grade (fork C2): passes on texture slots, run on the CPU by
    /// [`crate::grade::execute_graph_cpu`]. Built by `grade::compile_fx`, never from an effect.
    GradeGraph(std::sync::Arc<crate::grade::GradeProgram>),
}

/// Most parameter slots a user effect's uniform holds (`filmcraft_usershader::MAX_PARAMS`).
pub const USER_PARAM_SLOTS: usize = 16;

/// A user shader effect's evaluated pass ([`FxOp::User`]).
#[derive(Clone, Debug, PartialEq)]
pub struct UserOp {
    pub id: String,
    /// The source hash (a new hash is a new pipeline).
    pub hash: u64,
    /// The composed module (`rgba32float` storage).
    pub module: std::sync::Arc<str>,
    /// One slot per parameter in declaration order: float `[v, 0, 0, 0]`, bool `[0 | 1, …]`,
    /// colour straight sRGB `[r, g, b, 1]`, point in working pixels `[x, y, 0, 0]`.
    pub pv: Vec<[f32; 4]>,
    /// Seconds since the clip start.
    pub time: f32,
}

impl UserOp {
    /// Evaluate the registered user effect `e` at `cx` for a `w`×`h` working image. None: not a
    /// registered user effect (missing file: the effect renders as a pass-through).
    fn eval(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> Option<UserOp> {
        use filmcraft_project::ParamKind;
        let (hash, module) = filmcraft_project::effect::user_effect_module(&e.effect)?;
        let def = filmcraft_project::effect::find_effect(&e.effect)?;
        let img = Image { w, h, px: Vec::new() };
        let pv = def
            .params
            .iter()
            .take(USER_PARAM_SLOTS)
            .map(|p| match p.kind {
                ParamKind::Bool => [f32::from(u8::from(b(e, p.id))), 0.0, 0.0, 0.0],
                ParamKind::Color => {
                    let c = color(e, p.id, cx);
                    [c[0], c[1], c[2], 1.0]
                }
                ParamKind::Point => {
                    let v = point(e, p.id, cx, &img);
                    [v.x as f32, v.y as f32, 0.0, 0.0]
                }
                _ => [f(e, p.id, cx), 0.0, 0.0, 0.0],
            })
            .collect();
        Some(UserOp { id: e.effect.clone(), hash, module, pv, time: cx.seconds as f32 })
    }
}

/// A neighbouring frame of [`FxOp::TemporalDenoise`].
#[derive(Clone, Debug)]
pub enum NeighbourPicture {
    /// A source frame: decoded like the layer's own frame (box-decimated by `decimation`, then
    /// resampled to the working size, [`working_image`]) and run through the ops that precede
    /// the temporal op in the layer ([`apply_ops`]; the GPU replays the same passes). The GPU
    /// path needs this form.
    Frame { frame: std::sync::Arc<filmcraft_frame::VideoFrame>, decimation: u32 },
    /// A working image already in the state of the image being filtered (CPU path only:
    /// [`FxOp::gpu_ok`] is false while one is selected). Skipped when its size differs.
    Image(std::sync::Arc<Image>),
}

/// One neighbour: frame `t + offset` and its motion relative to the current frame.
#[derive(Clone, Debug)]
pub struct TemporalNeighbour {
    /// −radius…radius, not 0.
    pub offset: i8,
    pub picture: NeighbourPicture,
    /// Current → neighbour (`crate::flow::estimate(current, neighbour)`, ±2 by `flow::chain`).
    pub flow: std::sync::Arc<crate::flow::FlowField>,
    /// Neighbour → current.
    pub back: std::sync::Arc<crate::flow::FlowField>,
}

/// The neighbours [`FxOp::TemporalDenoise`] blends (see `tnr::select` for which are used).
#[derive(Clone, Debug, Default)]
pub struct TemporalInputs {
    /// Size of the current source frame: [`NeighbourPicture::Frame`]s of another size are skipped.
    pub source_size: (u32, u32),
    pub neighbours: Vec<TemporalNeighbour>,
}

impl PartialEq for NeighbourPicture {
    fn eq(&self, o: &Self) -> bool {
        match (self, o) {
            (NeighbourPicture::Frame { frame: a, decimation: da }, NeighbourPicture::Frame { frame: b, decimation: db }) => {
                std::sync::Arc::ptr_eq(a, b) && da == db
            }
            (NeighbourPicture::Image(a), NeighbourPicture::Image(b)) => std::sync::Arc::ptr_eq(a, b) || a == b,
            _ => false,
        }
    }
}

impl PartialEq for TemporalNeighbour {
    fn eq(&self, o: &Self) -> bool {
        self.offset == o.offset && self.picture == o.picture && self.flow == o.flow && self.back == o.back
    }
}

impl PartialEq for TemporalInputs {
    fn eq(&self, o: &Self) -> bool {
        self.source_size == o.source_size && self.neighbours == o.neighbours
    }
}

/// A source frame as the effect stage's working image of `size`: box-decimated by `decimation`
/// (as the CPU decodes it) and resampled when `size` is smaller still (the decode half of
/// `plan::effect_image`). Empty sizes give an empty image.
pub fn working_image(frame: &filmcraft_frame::VideoFrame, decimation: u32, size: (usize, usize)) -> Image {
    let (w, h, px) = frame.to_linear_f32_decimated(decimation.max(1) as usize);
    let mut img = Image { w, h, px };
    let (fw, fh) = size;
    if (fw, fh) != (w, h) && fw > 0 && fh > 0 && fw <= w && fh <= h {
        img = img.transformed(fw, fh, &Affine::scale(fw as f64 / w as f64, fh as f64 / h as f64));
    }
    img
}

/// Run a layer's ops in order on the CPU. Same as calling [`FxOp::apply`] one by one, except
/// that a [`FxOp::TemporalDenoise`] runs its source-frame neighbours through the ops before it
/// (what the GPU stage does; inside a masked effect: the layer's ops before the effect, then the
/// effect's own earlier ops), so `plan::effect_image` uses this.
pub fn apply_ops(ops: &[FxOp], img: &mut Image) {
    apply_ops_with(ops, img, &[], true);
}

/// Whether `ops` contain a temporal op with inputs (also inside masked effects).
fn has_temporal(ops: &[FxOp]) -> bool {
    ops.iter().any(|o| match o {
        FxOp::TemporalDenoise { inputs, .. } => inputs.is_some(),
        FxOp::Masked { ops, .. } | FxOp::MaskedBy { ops, .. } => has_temporal(ops),
        _ => false,
    })
}

/// [`apply_ops`] for ops that follow `outer` (the ops before an enclosing masked effect);
/// `temporal` false makes temporal ops identity (a neighbour's replay of the earlier ops).
pub(crate) fn apply_ops_with(ops: &[FxOp], img: &mut Image, outer: &[FxOp], temporal: bool) {
    for (j, op) in ops.iter().enumerate() {
        let before = || -> Vec<FxOp> { outer.iter().chain(ops.get(..j).unwrap_or_default()).cloned().collect() };
        match op {
            FxOp::TemporalDenoise { luma, chroma, radius, motion, inputs } => {
                if let (true, Some(inp)) = (temporal, inputs) {
                    crate::tnr::apply_inputs(img, inp, *luma, *chroma, *radius, *motion, &before());
                }
            }
            FxOp::Masked { ops: inner, masks } if has_temporal(inner) => {
                let original = img.clone();
                apply_ops_with(inner, img, &before(), temporal);
                if let Some(cov) = crate::mask::coverage(masks, img.w, img.h) {
                    crate::mask::mix(img, &original, &cov);
                }
            }
            FxOp::MaskedBy { ops: inner, matte } if has_temporal(inner) => {
                let outer = before();
                crate::matte::apply_masked(img, matte, |img| apply_ops_with(inner, img, &outer, temporal));
            }
            _ => op.apply(img),
        }
    }
}

/// Masks the GPU takes (more points or masks: the clip renders on the CPU).
pub const GPU_MASK_POINTS: usize = 1 << 16;
pub const GPU_MASKS: usize = 64;

/// Whether the mask shader can take `masks` (finite, within the limits).
pub(crate) fn masks_ok(masks: &[crate::mask::FlatMask]) -> bool {
    let active = masks.iter().filter(|m| m.mode != filmcraft_project::MaskMode::None);
    active.clone().count() <= GPU_MASKS
        && active.clone().map(|m| m.pts.len()).sum::<usize>() <= GPU_MASK_POINTS
        && active.into_iter().all(|m| [m.feather, m.expansion, m.opacity].iter().chain(m.pts.iter().flatten()).all(|v| v.is_finite()))
}

/// The ops of a masked effect the GPU runs: no nested mask (one saved image at a time), no
/// temporal op with inputs, each one [`FxOp::gpu_ok`].
fn inner_gpu_ok(ops: &[FxOp]) -> bool {
    ops.iter().all(|o| {
        !matches!(o, FxOp::Masked { .. } | FxOp::MaskedBy { .. } | FxOp::OpacityMask { .. } | FxOp::TemporalDenoise { inputs: Some(_), .. }) && o.gpu_ok()
    })
}

/// The largest median radius the GPU runs (a (2r+1)² window per pixel).
pub const GPU_MEDIAN_MAX: u32 = 3;

/// Effect ids [`FxOp::eval`] understands (the GPU-capable standard effects).
pub const GPU_EFFECTS: &[&str] = &[
    "brightness_contrast",
    "proc_amp",
    "tint",
    "black_white",
    "color_balance",
    "leave_color",
    "change_to_color",
    "color_pass",
    "gamma_correction",
    "levels",
    "extract",
    "invert",
    "posterize",
    "gaussian_blur",
    "gaussian_blur_legacy",
    "camera_blur",
    "directional_blur",
    "directional_blur_legacy",
    "sharpen",
    "unsharp_mask",
    "crop",
    "edge_feather",
    "transform",
    "horizontal_flip",
    "vertical_flip",
    "mirror",
    "offset",
    "asc_cdl",
    "channel_mix",
    "color_replace",
    "alpha_adjust",
    "luma_key",
    "ultra_key",
    "color_key",
    "vignette",
    "video_limiter",
];

/// `Image::transformed` without the mip path: the destination rectangle it writes and the inverse.
fn resample(w: usize, h: usize, m: &Affine, opacity: f32) -> Resample {
    let Some(inv) = m.inverse() else { return Resample { inv: None, rect: [0; 4], opacity } };
    let b = m.bounds(&filmcraft_geom::Rect::new(0.0, 0.0, w as f64, h as f64));
    let y0 = (b.y.floor().max(0.0) as usize).min(h);
    let y1 = (b.bottom().ceil().max(0.0) as usize).min(h);
    let x0 = (b.x.floor().max(0.0) as usize).min(w);
    let x1 = (b.right().ceil().max(0.0) as usize).min(w);
    Resample { inv: Some(inv), rect: [x0 as u32, x1 as u32, y0.min(y1) as u32, y1 as u32], opacity }
}

/// Whether `Image::transformed` would take its mip (downsample) path for `m` on a `w`×`h` image.
fn minifies(m: &Affine, w: usize, h: usize) -> bool {
    let sx = (m.a * m.a + m.b * m.b).sqrt();
    let sy = (m.c * m.c + m.d * m.d).sqrt();
    let minify = 1.0 / sx.min(sy).max(1e-6);
    // (a non-finite factor: keep such maps off the GPU; `apply` still hands them to the CPU's
    // `Image::transformed`, which decides as before)
    !minify.is_finite() || minify >= 2.0 && w >= 4 && h >= 4
}

/// Effects that become several [`FxOp`]s when the GPU can draw them ([`FxOp::eval_all`]).
pub const GPU_CHAINED_EFFECTS: &[&str] = &["lumetri"];

/// Whether [`FxOp::eval_all`] may give `effect` a GPU form (the effects' "GPU accelerated"
/// badge, [`filmcraft_project::effect::GPU_EFFECTS`]).
pub fn gpu_capable(effect: &str) -> bool {
    filmcraft_project::effect::GPU_EFFECTS.contains(&effect)
}

#[cfg(test)]
#[test]
fn the_badge_list_is_what_the_gpu_draws() {
    let mut ours: Vec<&str> = GPU_EFFECTS.iter().chain(GPU_CHAINED_EFFECTS).copied().collect();
    let mut badges = filmcraft_project::effect::GPU_EFFECTS.to_vec();
    ours.sort_unstable();
    badges.sort_unstable();
    assert_eq!(ours, badges);
    for d in filmcraft_project::effect_defs().iter().filter(|d| d.kind == filmcraft_project::EffectKind::Video && !d.intrinsic) {
        assert_eq!(d.accelerated, gpu_capable(d.id), "{}", d.id);
    }
}

/// Fuse runs of adjacent colour chains into one chain (one GPU pass instead of several).
pub fn fuse_color_chains(ops: Vec<FxOp>) -> Vec<FxOp> {
    let mut out: Vec<FxOp> = Vec::with_capacity(ops.len());
    for op in ops {
        if let (Some(FxOp::ColorChain(a)), FxOp::ColorChain(b)) = (out.last_mut(), &op) {
            let mut v = a.to_vec();
            v.extend(b.iter().cloned());
            *a = v.into();
            continue;
        }
        out.push(op);
    }
    out
}

impl FxOp {
    /// The GPU form of `e` at `cx.t` for a `w`×`h` working image: [`eval`](Self::eval)'s op, or
    /// the several ops of a chained effect (Lumetri: a colour chain and Sharpen). None: no GPU
    /// form (now), the effect is rendered on the CPU. Disabled effects give no ops.
    pub fn eval_all(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> Option<Vec<FxOp>> {
        if !e.enabled {
            return Some(Vec::new());
        }
        match e.effect.as_str() {
            "lumetri" => crate::effects::lumetri_gpu(e, cx, w, h),
            _ => FxOp::eval(e, cx, w, h).map(|op| vec![op]),
        }
    }

    /// Evaluate `e` at `cx.t` for a `w`×`h` working image. None for effects without a GPU
    /// implementation (and for disabled effects).
    pub fn eval(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> Option<FxOp> {
        if !e.enabled {
            return None;
        }
        Some(match e.effect.as_str() {
            "brightness_contrast" => FxOp::BrightnessContrast { br: f(e, "brightness", cx) / 100.0 * 0.4, co: 1.0 + f(e, "contrast", cx) / 100.0 },
            "proc_amp" => FxOp::ProcAmp {
                br: f(e, "brightness", cx) / 100.0 * 0.4,
                co: f(e, "contrast", cx) / 100.0,
                hue: f(e, "hue", cx) / 360.0,
                sat: f(e, "saturation", cx) / 100.0,
            },
            "tint" => {
                let (bl, wh) = (color(e, "black", cx), color(e, "white", cx));
                FxOp::Tint { black: [bl[0], bl[1], bl[2]], white: [wh[0], wh[1], wh[2]], amount: f(e, "amount", cx) / 100.0 }
            }
            "black_white" => FxOp::BlackWhite,
            "color_balance" => {
                let g = |k: &str| f(e, k, cx) / 100.0 * 0.25;
                FxOp::ColorBalance {
                    sh: [g("shadow_r"), g("shadow_g"), g("shadow_b")],
                    md: [g("mid_r"), g("mid_g"), g("mid_b")],
                    hi: [g("hi_r"), g("hi_g"), g("hi_b")],
                    preserve: b(e, "preserve"),
                }
            }
            "leave_color" => {
                let key = color(e, "color", cx);
                FxOp::LeaveColor {
                    amount: f(e, "amount", cx) / 100.0,
                    key_hue: rgb_to_hsl(key[0], key[1], key[2])[0],
                    tol: f(e, "tolerance", cx) / 100.0,
                    soft: f(e, "softness", cx) / 100.0 + 1e-4,
                }
            }
            "change_to_color" => {
                let (from, to) = (color(e, "from", cx), color(e, "to", cx));
                FxOp::ChangeToColor {
                    from_hue: rgb_to_hsl(from[0], from[1], from[2])[0],
                    to_hue: rgb_to_hsl(to[0], to[1], to[2])[0],
                    tol: f(e, "hue_tol", cx) / 100.0,
                    soft: f(e, "softness", cx) / 100.0 * 0.3 + 1e-4,
                }
            }
            "color_pass" => {
                let key = color(e, "color", cx);
                FxOp::ColorPass { key: [key[0], key[1], key[2]], sim: f(e, "similarity", cx) / 100.0, reverse: b(e, "reverse") }
            }
            "gamma_correction" => FxOp::Gamma { g: f(e, "gamma", cx) / 10.0 },
            "levels" => {
                let ib = f(e, "in_black", cx) / 255.0;
                FxOp::Levels {
                    ib,
                    iw: (f(e, "in_white", cx) / 255.0).max(ib + 1e-3),
                    ob: f(e, "out_black", cx) / 255.0,
                    ow: f(e, "out_white", cx) / 255.0,
                    g: 100.0 / f(e, "gamma", cx).max(1.0),
                }
            }
            "extract" => FxOp::Extract {
                lo: f(e, "black", cx) / 255.0,
                hi: f(e, "white", cx) / 255.0,
                soft: f(e, "softness", cx) / 100.0 * 0.2 + 1e-4,
                invert: b(e, "invert"),
            },
            "invert" => FxOp::Invert { channel: choice(e, "channel"), blend: f(e, "blend", cx) / 100.0 },
            "posterize" => FxOp::Posterize { n: f(e, "levels", cx).max(2.0) - 1.0 },
            "gaussian_blur" | "gaussian_blur_legacy" => {
                let r = f(e, "blurriness", cx) * cx.px_scale * 0.5;
                let dims = choice(e, "dimensions");
                let (rx, ry) = gaussian_boxes(w, h, if dims == 2 { 0.0 } else { r }, if dims == 1 { 0.0 } else { r });
                FxOp::Gaussian { rx, ry, repeat: b(e, "repeat_edge") }
            }
            "camera_blur" => {
                let r = f(e, "percent", cx) * cx.px_scale * 0.3;
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Gaussian { rx, ry, repeat: true }
            }
            "directional_blur" | "directional_blur_legacy" => {
                let len = f(e, "length", cx) * cx.px_scale * 2.0;
                let dir = (f(e, "direction", cx) as f64).to_radians();
                if len < 0.5 {
                    FxOp::DirectionalBlur { dx: 0.0, dy: 0.0, steps: 0 }
                } else {
                    let steps = (len.ceil() as usize).clamp(2, 64) as u32;
                    FxOp::DirectionalBlur { dx: (dir.sin() as f32) * len, dy: (-dir.cos() as f32) * len, steps }
                }
            }
            "sharpen" => {
                let r = cx.px_scale.max(0.35);
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Unsharp { rx, ry, amount: f(e, "amount", cx) / 100.0, threshold: 0.0 }
            }
            "unsharp_mask" => {
                let r = f(e, "radius", cx) * cx.px_scale;
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Unsharp { rx, ry, amount: f(e, "amount", cx) / 100.0, threshold: f(e, "threshold", cx) / 255.0 }
            }
            "crop" => {
                let l = f(e, "left", cx) / 100.0;
                let t = f(e, "top", cx) / 100.0;
                let r = f(e, "right", cx) / 100.0;
                let bt = f(e, "bottom", cx) / 100.0;
                if b(e, "zoom") && l + r < 0.99 && t + bt < 0.99 {
                    let (fw, fh) = (w as f64, h as f64);
                    let m = Affine::scale(1.0 / (1.0 - (l + r) as f64), 1.0 / (1.0 - (t + bt) as f64))
                        .then_apply(&Affine::translate(-(l as f64) * fw, -(t as f64) * fh));
                    if minifies(&m, w, h) {
                        // negative crops zoom out: the CPU's mip path (see `transform`)
                        return Some(FxOp::Resample(Resample { inv: Some(m), rect: [u32::MAX; 4], opacity: 1.0 }));
                    }
                    FxOp::Resample(resample(w, h, &m, 1.0))
                } else {
                    FxOp::crop_rect(w, h, l, t, r, bt, f(e, "feather", cx) * cx.px_scale)
                }
            }
            "edge_feather" => {
                let amt = f(e, "amount", cx) / 100.0 * (w.min(h) as f32) * 0.5;
                FxOp::crop_rect(w, h, 0.0, 0.0, 0.0, 0.0, amt)
            }
            "transform" => {
                let img = Image { w, h, px: Vec::new() };
                let anchor = point(e, "anchor", cx, &img);
                let pos = point(e, "position", cx, &img);
                let sh = f(e, "scale_height", cx) as f64 / 100.0;
                let sw = if b(e, "uniform_scale") { sh } else { f(e, "scale_width", cx) as f64 / 100.0 };
                let rot = f(e, "rotation", cx) as f64;
                let skew = (f(e, "skew", cx) as f64).to_radians().tan();
                let skew_axis = f(e, "skew_axis", cx) as f64;
                let op = f(e, "opacity", cx) / 100.0;
                let sk = Affine::rotate_deg(skew_axis)
                    .then_apply(&Affine { a: 1.0, b: 0.0, c: skew, d: 1.0, e: 0.0, f: 0.0 })
                    .then_apply(&Affine::rotate_deg(-skew_axis));
                let m = Affine::translate(pos.x, pos.y)
                    .then_apply(&Affine::rotate_deg(rot))
                    .then_apply(&sk)
                    .then_apply(&Affine::scale(sw, sh))
                    .then_apply(&Affine::translate(-anchor.x, -anchor.y));
                if minifies(&m, w, h) {
                    // the CPU's mip path (`Image::transformed` with the forward map; `rect`
                    // u32::MAX marks it): CPU only
                    return Some(FxOp::Resample(Resample { inv: Some(m), rect: [u32::MAX; 4], opacity: op }));
                }
                FxOp::Resample(resample(w, h, &m, op))
            }
            "horizontal_flip" => FxOp::HFlip,
            "vertical_flip" => FxOp::VFlip,
            "mirror" => {
                let img = Image { w, h, px: Vec::new() };
                let c = point(e, "center", cx, &img);
                let ang = (f(e, "angle", cx) as f64).to_radians();
                FxOp::Mirror { cx: c.x as f32, cy: c.y as f32, nx: ang.cos() as f32, ny: ang.sin() as f32 }
            }
            "offset" => {
                let img = Image { w, h, px: Vec::new() };
                let s = point(e, "shift", cx, &img);
                let (dx, dy) = (s.x - w as f64 / 2.0, s.y - h as f64 / 2.0);
                FxOp::Offset { dx: dx.rem_euclid(w.max(1) as f64) as f32, dy: dy.rem_euclid(h.max(1) as f64) as f32, blend: f(e, "blend", cx) / 100.0 }
            }
            "asc_cdl" => {
                use crate::vfx::fv;
                FxOp::AscCdl {
                    slope: [fv(e, "r_slope", cx), fv(e, "g_slope", cx), fv(e, "b_slope", cx)],
                    offset: [fv(e, "r_offset", cx), fv(e, "g_offset", cx), fv(e, "b_offset", cx)],
                    power: [fv(e, "r_power", cx), fv(e, "g_power", cx), fv(e, "b_power", cx)],
                    sat: fv(e, "saturation", cx),
                }
            }
            "channel_mix" => {
                use crate::vfx::{bv, fv};
                let g = |k: &str| fv(e, k, cx) / 100.0;
                let mut m = [[g("rr"), g("rg"), g("rb"), g("rc")], [g("gr"), g("gg"), g("gb"), g("gc")], [g("br"), g("bg"), g("bb"), g("bc")]];
                if bv(e, "monochrome") {
                    m = [m[0]; 3];
                }
                FxOp::ChannelMix { m }
            }
            "color_replace" => {
                use crate::vfx::{bv, cv, fv};
                let t = cv(e, "target", cx);
                let r = cv(e, "replace", cx);
                FxOp::ColorReplace {
                    sim: fv(e, "similarity", cx) / 100.0 * 1.2,
                    solid: bv(e, "solid"),
                    target: [t[0], t[1], t[2]],
                    replace: [r[0], r[1], r[2]],
                    replace_hsl: rgb_to_hsl(r[0], r[1], r[2]),
                }
            }
            "alpha_adjust" => {
                use crate::vfx::{bv, fv};
                FxOp::AlphaAdjust { opacity: fv(e, "opacity", cx) / 100.0, ignore: bv(e, "ignore"), invert: bv(e, "invert"), mask_only: bv(e, "mask_only") }
            }
            "ultra_key" => FxOp::UltraKey(Box::new(crate::vfx::color::ultra_key_eval(e, cx, w, h))),
            "color_key" => {
                let kc = color(e, "color", cx);
                FxOp::ColorKey {
                    k: filmcraft_color::rgb_to_ycbcr(kc[0], kc[1], kc[2], filmcraft_color::Matrix::Bt709),
                    tol: f(e, "tolerance", cx) / 255.0 * 0.6 + 0.01,
                    soft: f(e, "feather", cx) / 50.0 * 0.2 + 0.01,
                }
            }
            id if filmcraft_project::effect::is_user_effect(id) => FxOp::User(Box::new(UserOp::eval(e, cx, w, h)?)),
            "luma_key" => FxOp::LumaKey { th: f(e, "threshold", cx) / 100.0, cut: f(e, "cutoff", cx) / 100.0 },
            "vignette" => {
                use crate::vfx::{cv, fv};
                let amt = fv(e, "amount", cx) / 100.0;
                let mid = fv(e, "midpoint", cx) / 100.0;
                let round = fv(e, "roundness", cx) / 100.0;
                let raw_feather = fv(e, "feather", cx);
                let feather = if raw_feather.is_finite() { (raw_feather / 100.0).max(0.01) } else { raw_feather };
                let col = cv(e, "color", cx);
                let target = if amt < 0.0 { [col[0], col[1], col[2]] } else { [1.0; 3] };
                FxOp::Vignette { amount: amt, midpoint: mid, roundness: round, feather, target }
            }
            "video_limiter" => {
                use crate::vfx::{bv, cv};
                use filmcraft_project::ParamValue;
                let clip_level = match e.param("clip_level").map(|p| p.value.clone()).or_else(|| crate::vfx::def_value(e, "clip_level")) {
                    Some(ParamValue::Choice(c)) => c as f32,
                    Some(ParamValue::Float(f)) => f as f32,
                    _ => 0.0,
                };
                let comp_val = match e.param("compression").map(|p| p.value.clone()).or_else(|| crate::vfx::def_value(e, "compression")) {
                    Some(ParamValue::Choice(c)) => [0.0, 0.03, 0.05, 0.10, 0.20][(c as usize).min(4)],
                    Some(ParamValue::Float(f)) => f as f32,
                    _ => 0.03,
                };
                let axis = match e.param("axis").map(|p| p.value.clone()).or_else(|| crate::vfx::def_value(e, "axis")) {
                    Some(ParamValue::Choice(c)) => c,
                    Some(ParamValue::Float(f)) => {
                        if f.is_nan() {
                            u32::MAX
                        } else {
                            f as u32
                        }
                    }
                    _ => 3,
                };
                let warn = bv(e, "gamut_warning");
                let c = cv(e, "warning_color", cx);
                let wc = dec([c[0], c[1], c[2]]);
                FxOp::VideoLimiter { max: 1.0 + clip_level / 100.0, comp: comp_val, axis, warn, warning_color: wc }
            }
            _ => return None,
        })
    }

    fn crop_rect(w: usize, h: usize, l: f32, t: f32, r: f32, b: f32, feather: f32) -> FxOp {
        let (w, h) = (w as f32, h as f32);
        FxOp::Crop { x0: l * w, x1: w * (1.0 - r), y0: t * h, y1: h * (1.0 - b), feather: feather.max(0.0) }
    }

    /// Whether the GPU shader reproduces [`apply`](Self::apply) for this op: every number finite
    /// (the CPU's NaN / infinity behaviour is left to the CPU) and resamples that the CPU does
    /// not pre-filter.
    pub fn gpu_ok(&self) -> bool {
        let fin = |v: &[f32]| v.iter().all(|x| x.is_finite());
        match self {
            FxOp::BrightnessContrast { br, co } => fin(&[*br, *co]),
            FxOp::ProcAmp { br, co, hue, sat } => fin(&[*br, *co, *hue, *sat]),
            FxOp::Tint { black, white, amount } => fin(black) && fin(white) && amount.is_finite(),
            FxOp::BlackWhite | FxOp::HFlip | FxOp::VFlip => true,
            FxOp::ColorBalance { sh, md, hi, .. } => fin(sh) && fin(md) && fin(hi),
            FxOp::LeaveColor { amount, key_hue, tol, soft } => fin(&[*amount, *key_hue, *tol, *soft]),
            FxOp::ChangeToColor { from_hue, to_hue, tol, soft } => fin(&[*from_hue, *to_hue, *tol, *soft]),
            FxOp::ColorPass { key, sim, .. } => fin(key) && sim.is_finite(),
            FxOp::Gamma { g } => g.is_finite(),
            FxOp::Levels { ib, iw, ob, ow, g } => fin(&[*ib, *iw, *ob, *ow, *g]),
            FxOp::Extract { lo, hi, soft, .. } => fin(&[*lo, *hi, *soft]),
            FxOp::Invert { blend, .. } => blend.is_finite(),
            FxOp::Posterize { n } => n.is_finite(),
            FxOp::Gaussian { .. } => true,
            FxOp::DirectionalBlur { dx, dy, .. } => fin(&[*dx, *dy]),
            FxOp::Unsharp { amount, threshold, .. } => fin(&[*amount, *threshold]),
            FxOp::Crop { x0, x1, y0, y1, feather } => fin(&[*x0, *x1, *y0, *y1, *feather]),
            FxOp::Resample(r) => {
                r.rect[0] != u32::MAX
                    && r.opacity.is_finite()
                    && r.inv.as_ref().is_none_or(|m| [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite() && v.abs() < 1e7))
            }
            FxOp::Mirror { cx, cy, nx, ny } => fin(&[*cx, *cy, *nx, *ny]),
            FxOp::Offset { dx, dy, blend } => fin(&[*dx, *dy, *blend]),
            FxOp::AscCdl { slope, offset, power, sat } => fin(slope) && fin(offset) && fin(power) && sat.is_finite(),
            FxOp::ChannelMix { m } => m.iter().all(|r| fin(r)),
            FxOp::ColorReplace { sim, target, replace, replace_hsl, .. } => sim.is_finite() && fin(target) && fin(replace) && fin(replace_hsl),
            FxOp::AlphaAdjust { opacity, .. } => opacity.is_finite(),
            FxOp::LumaKey { th, cut } => fin(&[*th, *cut]),
            FxOp::ColorKey { k, tol, soft } => fin(k) && fin(&[*tol, *soft]),
            FxOp::UltraKey(k) => k.is_finite(),
            FxOp::HslSecondary { key, median_r, denoise, correct, .. } => {
                use filmcraft_color::ops::{ColorOp, MAX_GPU_PROGRAM, gpu_size};
                key.iter().chain(correct.iter()).all(ColorOp::is_finite)
                    && gpu_size(key) <= MAX_GPU_PROGRAM
                    && gpu_size(correct) <= MAX_GPU_PROGRAM
                    && *median_r <= GPU_MEDIAN_MAX
                    && denoise.is_finite()
            }
            FxOp::Masked { ops, masks } => masks_ok(masks) && inner_gpu_ok(ops),
            FxOp::MaskedBy { ops, matte } => {
                use crate::matte::GPU_MATTE_MAX;
                matte.is_valid() && matte.width <= GPU_MATTE_MAX && matte.height <= GPU_MATTE_MAX && inner_gpu_ok(ops)
            }
            FxOp::OpacityMask { masks } => masks_ok(masks),
            FxOp::ColorChain(ops) => {
                ops.iter().all(filmcraft_color::ops::ColorOp::is_finite) && filmcraft_color::ops::gpu_size(ops) <= filmcraft_color::ops::MAX_GPU_PROGRAM
            }
            FxOp::Vignette { amount, midpoint, roundness, feather, target } => fin(&[*amount, *midpoint, *roundness, *feather]) && fin(target),
            FxOp::VideoLimiter { max, comp, axis, warning_color, .. } => *axis <= 3 && max.is_finite() && comp.is_finite() && fin(warning_color),
            FxOp::User(u) => u.pv.len() <= USER_PARAM_SLOTS && u.pv.iter().all(|v| fin(v)) && u.time.is_finite(),
            // (fork C2 lane 3: `crates/gpu` runs the passes; what it can't take renders on the CPU)
            FxOp::GradeGraph(p) => p.gpu_ok(),
            // (`denoise::params` turns any values into finite constants, as the CPU does)
            FxOp::Denoise { .. } => true,
            // (the GPU draws source frames only; flows are checked by `tnr::select` on both)
            FxOp::TemporalDenoise { radius, inputs, .. } => inputs.as_ref().is_none_or(|inp| {
                crate::tnr::select(inp, *radius).iter().all(|&i| inp.neighbours.get(i).is_some_and(|n| matches!(n.picture, NeighbourPicture::Frame { .. })))
            }),
        }
    }

    /// The CPU reference.
    pub fn apply(&self, img: &mut Image) {
        if img.w == 0 || img.h == 0 {
            return;
        }
        match self {
            FxOp::BrightnessContrast { br, co } => {
                let (br, co) = (*br, *co);
                img.map_rgb(|c, _, _| {
                    let c = enc(c);
                    dec(c.map(|v| (v - 0.5) * co + 0.5 + br))
                });
            }
            FxOp::ProcAmp { br, co, hue, sat } => {
                let (br, co, hue, sat) = (*br, *co, *hue, *sat);
                img.map_rgb(|c, _, _| {
                    let c = enc(c);
                    let mut hsl = rgb_to_hsl(c[0], c[1], c[2]);
                    hsl[0] = (hsl[0] + hue).rem_euclid(1.0);
                    hsl[1] = (hsl[1] * sat).clamp(0.0, 1.0);
                    let c = hsl_to_rgb(hsl[0], hsl[1], hsl[2]);
                    dec(c.map(|v| (v - 0.5) * co + 0.5 + br))
                });
            }
            FxOp::Tint { black: bl, white: wh, amount } => {
                let amt = *amount;
                img.map_rgb(|c, _, _| {
                    let l = linear_to_srgb(luma709(c[0], c[1], c[2]).max(0.0));
                    let t = [bl[0] + (wh[0] - bl[0]) * l, bl[1] + (wh[1] - bl[1]) * l, bl[2] + (wh[2] - bl[2]) * l];
                    let e = enc(c);
                    dec([e[0] + (t[0] - e[0]) * amt, e[1] + (t[1] - e[1]) * amt, e[2] + (t[2] - e[2]) * amt])
                });
            }
            FxOp::BlackWhite => img.map_rgb(|c, _, _| {
                let l = luma709(c[0], c[1], c[2]);
                [l, l, l]
            }),
            FxOp::ColorBalance { sh, md, hi, preserve } => {
                let preserve = *preserve;
                img.map_rgb(|c, _, _| {
                    let c = enc(c);
                    let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                    let ws = (1.0 - l).powi(2);
                    let wh = l.powi(2);
                    let wm = 1.0 - ws - wh;
                    let mut o = [0.0; 3];
                    for k in 0..3 {
                        o[k] = c[k] + sh[k] * ws + md[k] * wm.max(0.0) + hi[k] * wh;
                    }
                    if preserve {
                        let l2 = 0.2126 * o[0] + 0.7152 * o[1] + 0.0722 * o[2];
                        let d = l - l2;
                        o = o.map(|v| v + d);
                    }
                    dec(o)
                });
            }
            FxOp::LeaveColor { amount, key_hue, tol, soft } => {
                let (amt, kh, tol, soft) = (*amount, *key_hue, *tol, *soft);
                img.map_rgb(|c, _, _| {
                    let ec = enc(c);
                    let h = rgb_to_hsl(ec[0], ec[1], ec[2])[0];
                    let d = (h - kh).abs().min(1.0 - (h - kh).abs()) * 2.0;
                    let keep = 1.0 - ((d - tol) / soft).clamp(0.0, 1.0);
                    let l = luma709(c[0], c[1], c[2]);
                    let k = amt * (1.0 - keep);
                    [c[0] + (l - c[0]) * k, c[1] + (l - c[1]) * k, c[2] + (l - c[2]) * k]
                });
            }
            FxOp::ChangeToColor { from_hue: fh, to_hue: th, tol, soft } => {
                let (fh, th, tol, soft) = (*fh, *th, *tol, *soft);
                img.map_rgb(|c, _, _| {
                    let ec = enc(c);
                    let mut hsl = rgb_to_hsl(ec[0], ec[1], ec[2]);
                    let d = (hsl[0] - fh).abs().min(1.0 - (hsl[0] - fh).abs());
                    let w = 1.0 - ((d - tol) / soft).clamp(0.0, 1.0);
                    hsl[0] = (hsl[0] + (th - fh) * w).rem_euclid(1.0);
                    dec(hsl_to_rgb(hsl[0], hsl[1], hsl[2]))
                });
            }
            FxOp::ColorPass { key, sim, reverse } => {
                let (sim, rev) = (*sim, *reverse);
                img.map_rgb(|c, _, _| {
                    let ec = enc(c);
                    let d = ((ec[0] - key[0]).powi(2) + (ec[1] - key[1]).powi(2) + (ec[2] - key[2]).powi(2)).sqrt();
                    let pass = (d <= sim * 1.2) != rev;
                    if pass {
                        c
                    } else {
                        let l = luma709(c[0], c[1], c[2]);
                        [l, l, l]
                    }
                });
            }
            FxOp::Gamma { g } => {
                let g = *g;
                img.map_rgb(|c, _, _| dec(enc(c).map(|v| v.max(0.0).powf(g))));
            }
            FxOp::Levels { ib, iw, ob, ow, g } => {
                let (ib, iw, ob, ow, g) = (*ib, *iw, *ob, *ow, *g);
                img.map_rgb(|c, _, _| dec(enc(c).map(|v| ob + (((v - ib) / (iw - ib)).clamp(0.0, 1.0)).powf(g) * (ow - ob))));
            }
            FxOp::Extract { lo, hi, soft, invert } => {
                let (lo, hi, soft, inv) = (*lo, *hi, *soft, *invert);
                img.map_rgb(|c, _, _| {
                    let l = linear_to_srgb(luma709(c[0], c[1], c[2]).max(0.0));
                    let inside = ((l - lo) / soft).clamp(0.0, 1.0).min(((hi - l) / soft).clamp(0.0, 1.0));
                    let v = if inv { 1.0 - inside } else { inside };
                    [v, v, v]
                });
            }
            FxOp::Invert { channel, blend } => {
                let (ch, blend) = (*channel, *blend);
                if ch == 4 {
                    img.px.par_chunks_mut(4).for_each(|p| {
                        let a = p[3];
                        let na = 1.0 - a;
                        let k = if a > 1e-6 { na / a } else { 0.0 };
                        for c in &mut p[..3] {
                            *c *= k;
                        }
                        p[3] = na * (1.0 - blend) + a * blend;
                    });
                } else {
                    img.map_rgb(|c, _, _| {
                        let ec = enc(c);
                        let mut o = ec;
                        for k in 0..3 {
                            if ch == 0 || ch as usize == k + 1 {
                                o[k] = 1.0 - ec[k];
                            }
                        }
                        dec([o[0] + (ec[0] - o[0]) * blend, o[1] + (ec[1] - o[1]) * blend, o[2] + (ec[2] - o[2]) * blend])
                    });
                }
            }
            FxOp::Posterize { n } => {
                let n = *n;
                img.map_rgb(|c, _, _| dec(enc(c).map(|v| (v * n).round() / n)));
            }
            FxOp::Gaussian { rx, ry, repeat } => crate::effects::box_blur(img, rx, ry, *repeat),
            FxOp::DirectionalBlur { dx, dy, steps } => crate::effects::directional_taps(img, *dx, *dy, *steps as usize),
            FxOp::Unsharp { rx, ry, amount, threshold } => crate::effects::unsharp_boxes(img, rx, ry, *amount, *threshold),
            FxOp::Crop { x0, x1, y0, y1, feather } => crate::effects::crop_px(img, *x0, *x1, *y0, *y1, *feather),
            FxOp::Resample(r) => {
                let out = match (&r.inv, r.rect[0]) {
                    (Some(m), u32::MAX) => img.transformed(img.w, img.h, m), // mip path (CPU only)
                    (Some(inv), _) => resample_cpu(img, inv, r.rect),
                    (None, _) => Image::new(img.w, img.h),
                };
                *img = out;
                img.scale_alpha(r.opacity);
            }
            FxOp::HFlip => {
                let w = img.w;
                img.px.par_chunks_mut(w * 4).for_each(|row| {
                    for x in 0..w / 2 {
                        for k in 0..4 {
                            row.swap(x * 4 + k, (w - 1 - x) * 4 + k);
                        }
                    }
                });
            }
            FxOp::VFlip => {
                let (w, h) = (img.w, img.h);
                for y in 0..h / 2 {
                    let (a, bb) = img.px.split_at_mut((h - 1 - y) * w * 4);
                    a[y * w * 4..(y + 1) * w * 4].swap_with_slice(&mut bb[..w * 4]);
                }
            }
            FxOp::Mirror { cx, cy, nx, ny } => {
                let (c, n) = ((*cx as f64, *cy as f64), (*nx as f64, *ny as f64));
                let src = img.clone();
                let w = img.w;
                img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                        let d = (px - c.0) * n.0 + (py - c.1) * n.1;
                        if d > 0.0 {
                            let (rx, ry) = (px - 2.0 * d * n.0, py - 2.0 * d * n.1);
                            row[x * 4..x * 4 + 4].copy_from_slice(&src.sample_bilinear(rx as f32, ry as f32));
                        }
                    }
                });
            }
            FxOp::Offset { dx, dy, blend } => {
                let (dx, dy, blend) = (*dx as f64, *dy as f64, *blend);
                let src = img.clone();
                let (w, h) = (img.w as f64, img.h as f64);
                let wi = img.w;
                img.px.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..wi {
                        let u = (x as f64 + 0.5 - dx).rem_euclid(w);
                        let v = (y as f64 + 0.5 - dy).rem_euclid(h);
                        let p = src.sample_bilinear_clamped(u as f32, v as f32);
                        let o = src.get(x, y);
                        for k in 0..4 {
                            row[x * 4 + k] = p[k] + (o[k] - p[k]) * blend;
                        }
                    }
                });
            }
            FxOp::AscCdl { slope, offset, power, sat } => {
                let (s, o, pw, sat) = (*slope, *offset, *power, *sat);
                img.map_rgb(|c, _, _| dec(crate::vfx::cdl(enc(c), s, o, pw, sat)));
            }
            FxOp::ChannelMix { m } => {
                let m = *m;
                img.map_rgb(|c, _, _| {
                    let v = enc(c);
                    dec([0, 1, 2].map(|k| (m[k][0] * v[0] + m[k][1] * v[1] + m[k][2] * v[2] + m[k][3]).clamp(0.0, 1.0)))
                });
            }
            FxOp::ColorReplace { sim, solid, target: t, replace: r, replace_hsl: rh } => {
                let (sim, solid) = (*sim, *solid);
                img.map_rgb(|c, _, _| {
                    let v = enc(c);
                    let d = ((v[0] - t[0]).powi(2) + (v[1] - t[1]).powi(2) + (v[2] - t[2]).powi(2)).sqrt();
                    let k = 1.0 - crate::vfx::smoothstep(sim * 0.85, sim.max(1e-4), d);
                    if k <= 0.0 {
                        return c;
                    }
                    let target = if solid {
                        [r[0], r[1], r[2]]
                    } else {
                        let l = rgb_to_hsl(v[0], v[1], v[2])[2];
                        hsl_to_rgb(rh[0], rh[1], l)
                    };
                    dec(crate::vfx::lerp3(v, target, k))
                });
            }
            FxOp::AlphaAdjust { opacity, ignore, invert, mask_only } => {
                let (op, ignore, invert, mask_only) = (*opacity, *ignore, *invert, *mask_only);
                img.px.par_chunks_mut(4).for_each(|p| {
                    let c = Image::unpremul([p[0], p[1], p[2], p[3]]);
                    let mut a = if ignore { 1.0 } else { p[3] };
                    if invert {
                        a = 1.0 - a;
                    }
                    a = (a * op).clamp(0.0, 1.0);
                    if mask_only {
                        let g = filmcraft_color::srgb_to_linear(a);
                        p.copy_from_slice(&[g, g, g, 1.0]);
                    } else {
                        p.copy_from_slice(&[c[0] * a, c[1] * a, c[2] * a, a]);
                    }
                });
            }
            FxOp::UltraKey(k) => crate::vfx::color::ultra_key_apply(img, k),
            FxOp::ColorKey { k, tol, soft } => {
                let (k, tol, soft) = (*k, *tol, *soft);
                img.px.par_chunks_mut(4).for_each(|p| {
                    if p[3] <= 0.0 {
                        return;
                    }
                    let c = enc([p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
                    let ycc = filmcraft_color::rgb_to_ycbcr(c[0], c[1], c[2], filmcraft_color::Matrix::Bt709);
                    let d = ((ycc[1] - k[1]).powi(2) + (ycc[2] - k[2]).powi(2)).sqrt() + (ycc[0] - k[0]).abs() * 0.15;
                    let a = p[3] * ((d - tol) / soft).clamp(0.0, 1.0);
                    let lo = dec(c);
                    p.copy_from_slice(&[lo[0] * a, lo[1] * a, lo[2] * a, a]);
                });
            }
            FxOp::LumaKey { th, cut } => {
                let (th, cut) = (*th, *cut);
                img.px.par_chunks_mut(4).for_each(|p| {
                    if p[3] <= 0.0 {
                        return;
                    }
                    let l = linear_to_srgb(luma709(p[0] / p[3], p[1] / p[3], p[2] / p[3]).max(0.0));
                    let a = if l <= cut {
                        0.0
                    } else if l >= th.max(cut + 1e-3) {
                        1.0
                    } else {
                        (l - cut) / (th - cut).max(1e-3)
                    };
                    for v in p.iter_mut() {
                        *v *= a;
                    }
                });
            }
            FxOp::ColorChain(ops) => img.map_rgb(|c, x, y| filmcraft_color::ops::apply_chain_at(ops, c, [x as f32, y as f32])),
            FxOp::Masked { ops, masks } => {
                let original = img.clone();
                for op in ops {
                    op.apply(img);
                }
                if let Some(cov) = crate::mask::coverage(masks, img.w, img.h) {
                    crate::mask::mix(img, &original, &cov);
                }
            }
            FxOp::MaskedBy { ops, matte } => crate::matte::apply_masked(img, matte, |img| ops.iter().for_each(|op| op.apply(img))),
            FxOp::OpacityMask { masks } => {
                if let Some(cov) = crate::mask::coverage(masks, img.w, img.h) {
                    crate::mask::scale_by(img, &cov);
                }
            }
            FxOp::HslSecondary { key, median_r, denoise, blur, correct } => {
                let (w, h) = (img.w, img.h);
                let mut mask: Vec<f32> = img
                    .px
                    .par_chunks(4)
                    .enumerate()
                    .map(|(i, p)| {
                        if p[3] <= 1e-6 {
                            return 0.0;
                        }
                        let xy = [(i % w) as f32, (i / w) as f32];
                        filmcraft_color::ops::apply_chain_px(key, Image::unpremul([p[0], p[1], p[2], p[3]]), xy, 1.0).1
                    })
                    .collect();
                if *denoise > 0.0 && *median_r > 0 {
                    let med = crate::effects::median_filter(&mask, w, h, *median_r as usize);
                    mask.iter_mut().zip(med).for_each(|(m, d)| *m += (d - *m) * denoise.min(1.0));
                }
                if !blur.is_empty() {
                    let radii: Vec<usize> = blur.iter().map(|r| *r as usize).collect();
                    crate::effects::blur_plane_radii(&mut mask, w, h, &radii);
                }
                let mask = &mask;
                img.map_rgb(|c, x, y| {
                    let m = mask.get(y * w + x).copied().unwrap_or(0.0);
                    filmcraft_color::ops::apply_chain_px(correct, c, [x as f32, y as f32], m).0
                });
            }
            FxOp::Vignette { amount, midpoint, roundness, feather, target } => {
                let (amt, mid, round, feather, target) = (*amount, *midpoint, *roundness, *feather, *target);
                if !amt.is_finite() || !mid.is_finite() || !round.is_finite() || !feather.is_finite() || !target.iter().all(|c| c.is_finite()) {
                    return;
                }
                if amt.abs() < 1e-5 {
                    return;
                }
                let (w, h) = (img.w as f32, img.h as f32);
                let aspect = w / h;
                img.map_rgb(|c, x, y| {
                    let mut nx = (x as f32 + 0.5) / w * 2.0 - 1.0;
                    let ny = (y as f32 + 0.5) / h * 2.0 - 1.0;
                    // roundness 100 = circle; 0 = an ellipse following the frame; −100 = squarer
                    if round > 0.0 {
                        nx *= 1.0 + (aspect - 1.0) * round;
                    }
                    let p = if round < 0.0 { 2.0 + (-round) * 6.0 } else { 2.0 };
                    // 0 at the centre, 1 at the corners
                    let d = (nx.abs().powf(p) + ny.abs().powf(p)).powf(1.0 / p) / 2f32.powf(1.0 / p);
                    let edge = crate::vfx::smoothstep(mid - feather * 0.5, mid + feather * 0.5, d);
                    dec(crate::vfx::lerp3(enc(c), target, edge * amt.abs()))
                });
            }
            // (GPU only, ADR 0006: the CPU leaves the image as it is)
            FxOp::User(_) => {}
            FxOp::GradeGraph(p) => *img = crate::grade::execute_graph_cpu(p, img),
            FxOp::VideoLimiter { max, comp, axis, warn, warning_color } => {
                let (max, comp, axis, warn, wc) = (*max, *comp, *axis, *warn, *warning_color);
                if !max.is_finite() || !comp.is_finite() || !wc.iter().all(|c| c.is_finite()) {
                    return;
                }
                img.map_rgb(|c, _, _| {
                    let v = [linear_to_enc(c[0]), linear_to_enc(c[1]), linear_to_enc(c[2])];
                    let out = limit(v, max, comp, axis);
                    if warn && v.iter().zip(&out).any(|(a, b)| (a - b).abs() > 1e-4) {
                        return wc;
                    }
                    out.map(enc_to_linear)
                });
            }
            FxOp::Denoise { luma, chroma, sigma } => crate::denoise::apply(img, *luma, *chroma, *sigma),
            // (alone, source-frame neighbours are not run through earlier ops: see `apply_ops`)
            FxOp::TemporalDenoise { luma, chroma, radius, motion, inputs } => {
                if let Some(inp) = inputs {
                    crate::tnr::apply_inputs(img, inp, *luma, *chroma, *radius, *motion, &[]);
                }
            }
        }
    }
}

/// Horizontal scale of the Lumetri vignette's coordinates for a Roundness of `vround`
/// (−1 … 1) on a frame `aspect` wide. At 0 the vignette is an ellipse that follows the
/// frame; positive values move it towards a circle (1 = a true circle), negative values
/// stretch it into a wider oval. Non-finite or out-of-range input never yields NaN.
pub fn lumetri_vignette_aspect(aspect: f32, vround: f32) -> f32 {
    if !aspect.is_finite() || aspect <= 0.0 || !vround.is_finite() {
        return 1.0;
    }
    aspect.powf(vround.clamp(-1.0, 1.0))
}

/// sRGB encoding that keeps values above 1 (super-whites) instead of clamping them.
#[inline]
fn linear_to_enc(v: f32) -> f32 {
    if v <= 1.0 { filmcraft_color::linear_to_srgb(v.max(0.0)) } else { 1.0 + (v - 1.0) / 2.4 }
}
#[inline]
fn enc_to_linear(v: f32) -> f32 {
    if v <= 1.0 { filmcraft_color::srgb_to_linear(v.max(0.0)) } else { 1.0 + (v - 1.0) * 2.4 }
}

#[inline]
fn knee(v: f32, max: f32, comp: f32) -> f32 {
    let k = max * (1.0 - comp);
    if comp <= 0.0 || v <= k {
        return v.min(max);
    }
    let r = max - k;
    k + r * (1.0 - (-(v - k) / r).exp())
}

pub(crate) fn limit(v: [f32; 3], max: f32, comp: f32, axis: u32) -> [f32; 3] {
    let m = filmcraft_color::Matrix::Bt709;
    let ycc = filmcraft_color::rgb_to_ycbcr(v[0], v[1], v[2], m);
    let (kr, kb) = (0.2126f32, 0.0722f32);
    let to_rgb = |y: f32, cb: f32, cr: f32| {
        let r = y + 2.0 * (1.0 - kr) * cr;
        let b = y + 2.0 * (1.0 - kb) * cb;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b]
    };
    let (mut y, mut cb, mut cr) = (ycc[0], ycc[1], ycc[2]);
    if axis != 1 {
        y = knee(y.max(0.0), max, comp);
    }
    if axis != 0 {
        // scale chroma so every channel fits 0…max (the gamut of legal R′G′B′)
        let rgb = to_rgb(y, cb, cr);
        let mut s = 1.0f32;
        for &ch in &rgb {
            let d = ch - y;
            if ch > max && d > 1e-6 {
                s = s.min(((max - y) / d).max(0.0));
            }
            if ch < 0.0 && d < -1e-6 {
                s = s.min((y / -d).max(0.0));
            }
        }
        if axis == 3 && s < 1.0 - 1e-4 {
            // smart limit: soft chroma compression rather than a hard cut
            s = knee(s, 1.0, comp.max(0.03)).min(1.0);
        }
        cb *= s;
        cr *= s;
    }
    to_rgb(y, cb, cr).map(|q| q.clamp(0.0, max))
}

/// The body of `Image::transformed` (no mip path) for a precomputed inverse and rectangle.
fn resample_cpu(src: &Image, inv: &Affine, rect: [u32; 4]) -> Image {
    let (w, h) = (src.w, src.h);
    let mut out = Image::new(w, h);
    let [x0, x1, y0, y1] = rect.map(|v| v as usize);
    let (x1, y1) = (x1.min(w), y1.min(h));
    let (x0, y0) = (x0.min(x1), y0.min(y1));
    let axis_aligned = inv.b == 0.0 && inv.c == 0.0;
    out.px.par_chunks_mut(w * 4).enumerate().skip(y0).take(y1 - y0).for_each(|(y, row)| {
        let py = y as f64 + 0.5;
        for x in x0..x1 {
            let px = x as f64 + 0.5;
            let (u, v) =
                if axis_aligned { (inv.a * px + inv.e, inv.d * py + inv.f) } else { (inv.a * px + inv.c * py + inv.e, inv.b * px + inv.d * py + inv.f) };
            if u < -1.0 || v < -1.0 || u > w as f64 + 1.0 || v > h as f64 + 1.0 {
                continue;
            }
            row[x * 4..x * 4 + 4].copy_from_slice(&src.sample_bilinear(u as f32, v as f32));
        }
    });
    out
}
