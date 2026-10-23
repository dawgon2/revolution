//! SAM 2.1 (Meta, Ravi et al. 2024; code and checkpoints Apache-2.0) as Magic Mask's video
//! tracker (fork G3.4, FORK-PLAN §1a.124): one port runs both official checkpoints, **Better =
//! Hiera Large** and **Faster = Hiera Base+**, driven by their YAML configs ([`Sam2Config`]).
//!
//! The model in four parts, as the reference `SAM2Base` / `SAM2VideoPredictor` runs it:
//! 1. **image encoder**: Hiera trunk + FPN neck → per frame a 64 × 64 × 256 embedding plus two
//!    high-resolution maps (256² × 32, 128² × 64) for the decoder ([`Sam2Features`]);
//! 2. **memory attention**: the embedding attends to a memory bank (the prompted frames, the six
//!    most recent frames, and up to 16 object pointers), 4 layers with 2-D axial RoPE;
//! 3. **prompt encoder + mask decoder** (SAM's two-way transformer, plus SAM 2's high-res skip
//!    and object-score head): the mask, its predicted IoU, whether the object is visible, and
//!    the object pointer;
//! 4. **memory encoder**: the frame's embedding fused with its mask → a 64 × 64 × 64 memory.
//!
//! Parts 1, 2 and 4 are the heavy ones and sit behind [`Sam2Compute`] (the GPU passes in
//! `filmcraft-gpu::sam2` implement it in the engine; [`cpu::Sam2Cpu`] is the CPU reference, used
//! as the parity oracle and the fallback). Part 3 is small and runs on the CPU via candle
//! ([`heads`]). [`tracker::Sam2Tracker`] is the video predictor's state machine.
//!
//! Ported from github.com/facebookresearch/sam2 (`sam2/modeling/**`, `sam2_video_predictor.py`),
//! Copyright (c) Meta Platforms, Inc. and affiliates, Apache License 2.0 (see `NOTICE`). The
//! structure follows the reference closely so the official checkpoints load unchanged.
//!
//! Plain-data layouts used across the interface (all f32, row-major): token-major / HWC, i.e.
//! `[y][x][channel]`.

pub mod config;
#[cfg(feature = "sam")]
pub mod cpu;
#[cfg(feature = "sam")]
pub mod heads;
#[cfg(feature = "sam")]
pub mod tracker;
#[cfg(feature = "sam")]
pub mod weights;

pub use config::{BlockSpec, D_MODEL, EMBED_SIDE, IMAGE_SIZE, MAX_OBJ_PTRS, MEM_DIM, Sam2Config, Sam2Size};

use crate::{MaskError, RgbImage};

/// Tokens in the 64 × 64 embedding.
pub const EMBED_TOKENS: usize = EMBED_SIDE * EMBED_SIDE;
/// The high-resolution maps' sides and widths.
pub const S0_SIDE: usize = 256;
pub const S0_DIM: usize = 32;
pub const S1_SIDE: usize = 128;
pub const S1_DIM: usize = 64;
/// Values in the encoder input (3 × 1024 × 1024, CHW).
pub const INPUT_LEN: usize = 3 * IMAGE_SIZE * IMAGE_SIZE;
/// Side of the mask the memory encoder takes (the image size).
pub const MEM_MASK_SIDE: usize = IMAGE_SIZE;

/// One frame through the image encoder (all HWC).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sam2Features {
    /// `conv_s0(fpn level 0)`: 256 × 256 × 32.
    pub s0: Vec<f32>,
    /// `conv_s1(fpn level 1)`: 128 × 128 × 64.
    pub s1: Vec<f32>,
    /// FPN level 2, the image embedding: 64 × 64 × 256.
    pub feat: Vec<f32>,
}

impl Sam2Features {
    /// Refuses wrong lengths and non-finite values (from a GPU that misbehaved).
    pub fn check(&self) -> Result<(), MaskError> {
        let lens = [(self.s0.len(), S0_SIDE * S0_SIDE * S0_DIM), (self.s1.len(), S1_SIDE * S1_SIDE * S1_DIM), (self.feat.len(), EMBED_TOKENS * D_MODEL)];
        if lens.iter().any(|(a, b)| a != b) {
            return Err(MaskError::Failed(format!("image features of {lens:?} values")));
        }
        if self.s0.iter().chain(&self.s1).chain(&self.feat).any(|v| !v.is_finite()) {
            return Err(MaskError::Failed("the image features have non-finite values".into()));
        }
        Ok(())
    }
}

/// The heavy parts of SAM 2 (image encoder, memory attention, memory encoder) on some device.
/// Plain data in, plain data out (HWC), so a GPU implementation needs no candle.
pub trait Sam2Compute: Send + Sync {
    /// For logs: where it runs.
    fn name(&self) -> String;
    /// The image encoder on a prepared input ([`prepare_input`]: 3 × 1024 × 1024 CHW).
    fn image(&self, input: &[f32]) -> Result<Sam2Features, MaskError>;
    /// Memory attention: the frame's embedding (`feat`, 4096 × 256) attends to `memory`
    /// (`n` × 64 tokens with their position encodings `memory_pos`); the last `ptr_tokens` of
    /// them are object pointers (no RoPE). Returns the conditioned embedding (4096 × 256).
    fn memory_attention(&self, feat: &[f32], memory: &[f32], memory_pos: &[f32], ptr_tokens: usize) -> Result<Vec<f32>, MaskError>;
    /// Memory encoder: the embedding (`feat`, 4096 × 256) with the mask for memory
    /// (`mask`: 1024 × 1024, already `sigmoid · scale + bias`) → 4096 × 64 memory features.
    fn memory_encoder(&self, feat: &[f32], mask: &[f32]) -> Result<Vec<f32>, MaskError>;
}

/// The frame as SAM 2 sees it: squashed to 1024 × 1024 (SAM 2 does not keep the aspect ratio;
/// an area average when shrinking so a 6.2K frame doesn't alias, bilinear when growing), scaled
/// to 0..1 and normalised with ImageNet's mean / std: 3 × 1024 × 1024, CHW.
pub fn prepare_input(image: &RgbImage) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let (w, h) = (image.width().max(1), image.height().max(1));
    let n = IMAGE_SIZE;
    let plane = n * n;
    let mut buf = vec![0f32; 3 * plane];
    let data = image.data();
    let at = |x: usize, y: usize, c: usize| data.get((y * w + x) * 3 + c).copied().unwrap_or(0) as f32;
    let (sx, sy) = (w as f32 / n as f32, h as f32 / n as f32);
    // source span [a, b) of output pixel i along an axis of `len` source pixels at scale s
    let span = |i: usize, s: f32, len: usize| {
        let a = ((i as f32 * s) as usize).min(len - 1);
        let b = (((i + 1) as f32 * s).ceil() as usize).clamp(a + 1, len);
        (a, b)
    };
    // bilinear tap along an axis
    let tap = |i: usize, s: f32, len: usize| {
        let f = ((i as f32 + 0.5) * s - 0.5).clamp(0.0, (len - 1) as f32);
        let i0 = f.floor() as usize;
        (i0, (i0 + 1).min(len - 1), f - i0 as f32)
    };
    for y in 0..n {
        for x in 0..n {
            for c in 0..3 {
                let v = if sx > 1.0 || sy > 1.0 {
                    let ((xa, xb), (ya, yb)) = (span(x, sx, w), span(y, sy, h));
                    let mut sum = 0.0;
                    for yy in ya..yb {
                        for xx in xa..xb {
                            sum += at(xx, yy, c);
                        }
                    }
                    sum / ((xb - xa) * (yb - ya)).max(1) as f32
                } else {
                    let ((x0, x1, fx), (y0, y1, fy)) = (tap(x, sx, w), tap(y, sy, h));
                    let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
                    let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
                    top * (1.0 - fy) + bottom * fy
                };
                if let Some(slot) = buf.get_mut(c * plane + y * n + x) {
                    *slot = (v / 255.0 - MEAN[c]) / STD[c];
                }
            }
        }
    }
    buf
}

/// `PositionEmbeddingSine(num_pos_feats = feats, normalize, scale 2π, temperature 10000)` on an
/// `h` × `w` grid, HWC (`[y][x][feats]`; the first half of the channels from y, then x).
pub fn sine_position(h: usize, w: usize, feats: usize) -> Vec<f32> {
    let npf = (feats / 2).max(1);
    let scale = 2.0 * std::f64::consts::PI;
    let dim_t: Vec<f64> = (0..npf).map(|i| 10_000f64.powf(2.0 * (i / 2) as f64 / npf as f64)).collect();
    let mut out = Vec::with_capacity(h * w * feats);
    let enc = |v: f64, out: &mut Vec<f32>| {
        for (i, d) in dim_t.iter().enumerate() {
            let a = v / d;
            out.push(if i % 2 == 0 { a.sin() } else { a.cos() } as f32);
        }
    };
    for y in 0..h {
        for x in 0..w {
            let ye = (y + 1) as f64 / (h as f64 + 1e-6) * scale;
            let xe = (x + 1) as f64 / (w as f64 + 1e-6) * scale;
            enc(ye, &mut out);
            enc(xe, &mut out);
        }
    }
    out
}

/// `get_1d_sine_pe(pos, dim)`: all sines, then all cosines.
pub fn sine_1d(pos: f32, dim: usize) -> Vec<f32> {
    let half = (dim / 2).max(1);
    let a: Vec<f64> = (0..half).map(|i| f64::from(pos) / 10_000f64.powf(2.0 * (i / 2) as f64 / half as f64)).collect();
    a.iter().map(|v| v.sin() as f32).chain(a.iter().map(|v| v.cos() as f32)).collect()
}

/// 2-D axial RoPE tables (`compute_axial_cis`) for a `side` × `side` grid and head width `dim`:
/// (cos, sin), each `[side²][dim / 2]`; pair `j` of token `t` turns by `x·f_j` (first half of
/// the pairs) or `y·f_j` (second half), `f_j = θ^(−4j/dim)`.
pub fn rope_tables(side: usize, dim: usize, theta: f32) -> (Vec<f32>, Vec<f32>) {
    let quarter = dim / 4;
    let freqs: Vec<f64> = (0..quarter).map(|j| 1.0 / f64::from(theta).powf((4 * j) as f64 / dim as f64)).collect();
    let pairs = dim / 2;
    let (mut c, mut s) = (Vec::with_capacity(side * side * pairs), Vec::with_capacity(side * side * pairs));
    for t in 0..side * side {
        let (x, y) = ((t % side) as f64, (t / side) as f64);
        for j in 0..pairs {
            let a = if j < quarter { x * freqs.get(j).copied().unwrap_or(0.0) } else { y * freqs.get(j - quarter).copied().unwrap_or(0.0) };
            // torch computes the angle in f32 before cos / sin
            let a = f64::from(a as f32);
            c.push(a.cos() as f32);
            s.push(a.sin() as f32);
        }
    }
    (c, s)
}

/// Rotates `rows` rows of width `dim` in place with the RoPE tables (row `r` uses table row
/// `r % period`): the reference's `apply_rotary_enc` (pairs `(2j, 2j+1)` as complex numbers).
pub fn apply_rope(x: &mut [f32], rows: usize, dim: usize, cos: &[f32], sin: &[f32], period: usize) {
    let pairs = dim / 2;
    for r in 0..rows {
        let t = r % period.max(1);
        for j in 0..pairs {
            let (Some(c), Some(s)) = (cos.get(t * pairs + j), sin.get(t * pairs + j)) else { continue };
            let i = r * dim + 2 * j;
            let (Some(a), Some(b)) = (x.get(i).copied(), x.get(i + 1).copied()) else { continue };
            if let Some(v) = x.get_mut(i) {
                *v = a * c - b * s;
            }
            if let Some(v) = x.get_mut(i + 1) {
                *v = a * s + b * c;
            }
        }
    }
}

/// PyTorch's bicubic resize (`F.interpolate(mode="bicubic", align_corners=False)`, A = −0.75,
/// edge-clamped taps) of a `c`-channel CHW grid to `oh` × `ow`, CHW.
pub fn bicubic_chw(src: &[f32], c: usize, h: usize, w: usize, oh: usize, ow: usize) -> Vec<f32> {
    const A: f64 = -0.75;
    let cc1 = |x: f64| ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0;
    let cc2 = |x: f64| ((A * x - 5.0 * A) * x + 8.0 * A) * x - 4.0 * A;
    let taps = |o: usize, inp: usize, out: usize| -> ([usize; 4], [f64; 4]) {
        let scale = inp as f64 / out as f64;
        let real = (o as f64 + 0.5) * scale - 0.5;
        let i0 = real.floor();
        let t = real - i0;
        let idx = |d: i64| (i0 as i64 + d).clamp(0, inp as i64 - 1) as usize;
        ([idx(-1), idx(0), idx(1), idx(2)], [cc2(t + 1.0), cc1(t), cc1(1.0 - t), cc2(2.0 - t)])
    };
    let mut out = vec![0f32; c * oh * ow];
    if h == 0 || w == 0 {
        return out;
    }
    let ys: Vec<_> = (0..oh).map(|y| taps(y, h, oh)).collect();
    let xs: Vec<_> = (0..ow).map(|x| taps(x, w, ow)).collect();
    for ch in 0..c {
        for (oy, (yi, yw)) in ys.iter().enumerate() {
            for (ox, (xi, xw)) in xs.iter().enumerate() {
                let mut v = 0.0f64;
                for (a, wy) in yi.iter().zip(yw) {
                    let mut row = 0.0f64;
                    for (b, wx) in xi.iter().zip(xw) {
                        row += f64::from(src.get((ch * h + a) * w + b).copied().unwrap_or(0.0)) * wx;
                    }
                    v += row * wy;
                }
                if let Some(slot) = out.get_mut((ch * oh + oy) * ow + ox) {
                    *slot = v as f32;
                }
            }
        }
    }
    out
}

/// Hiera's position embedding for the 256 × 256 token grid, HWC: the background embedding
/// (`pos_embed`, `c` × `bh` × `bw`) bicubically resized, plus the window embedding
/// (`pos_embed_window`, `c` × `ws` × `ws`) tiled.
pub fn hiera_position(bkg: &[f32], bh: usize, bw: usize, window: &[f32], ws: usize, c: usize) -> Vec<f32> {
    let side = 256;
    let big = bicubic_chw(bkg, c, bh, bw, side, side);
    let ws = ws.max(1);
    let mut out = vec![0f32; side * side * c];
    for y in 0..side {
        for x in 0..side {
            for ch in 0..c {
                let v = big.get((ch * side + y) * side + x).copied().unwrap_or(0.0) + window.get((ch * ws + y % ws) * ws + x % ws).copied().unwrap_or(0.0);
                if let Some(slot) = out.get_mut((y * side + x) * c + ch) {
                    *slot = v;
                }
            }
        }
    }
    out
}

/// The positional encoding of a 256 × 256 mask-grid point for the prompt encoder lives in
/// [`heads`]; this is the dense one (`PositionEmbeddingRandom` over the 64 × 64 grid), CHW.
pub fn dense_pe(gauss: &[f32], side: usize) -> Vec<f32> {
    let feats = gauss.len() / 2;
    let mut out = vec![0f32; 2 * feats * side * side];
    for y in 0..side {
        for x in 0..side {
            let cx = 2.0 * ((x as f32 + 0.5) / side as f32) - 1.0;
            let cy = 2.0 * ((y as f32 + 0.5) / side as f32) - 1.0;
            for f in 0..feats {
                let g0 = gauss.get(f).copied().unwrap_or(0.0);
                let g1 = gauss.get(feats + f).copied().unwrap_or(0.0);
                let a = 2.0 * std::f32::consts::PI * (cx * g0 + cy * g1);
                if let Some(s) = out.get_mut((f * side + y) * side + x) {
                    *s = a.sin();
                }
                if let Some(s) = out.get_mut(((feats + f) * side + y) * side + x) {
                    *s = a.cos();
                }
            }
        }
    }
    out
}

/// Bilinear resize (half-pixel centres, PyTorch `align_corners=False`, no antialias) of one
/// `h` × `w` plane to `oh` × `ow`.
pub fn bilinear_plane(src: &[f32], h: usize, w: usize, oh: usize, ow: usize) -> Vec<f32> {
    let mut out = vec![0f32; oh * ow];
    if h == 0 || w == 0 {
        return out;
    }
    let tap = |o: usize, inp: usize, out: usize| {
        let f = ((o as f32 + 0.5) * inp as f32 / out as f32 - 0.5).max(0.0);
        let i0 = (f as usize).min(inp - 1);
        (i0, (i0 + 1).min(inp - 1), f - i0 as f32)
    };
    let xs: Vec<_> = (0..ow).map(|x| tap(x, w, ow)).collect();
    for y in 0..oh {
        let (y0, y1, fy) = tap(y, h, oh);
        for (x, (x0, x1, fx)) in xs.iter().enumerate() {
            let at = |yy: usize, xx: usize| src.get(yy * w + xx).copied().unwrap_or(0.0);
            let top = at(y0, *x0) * (1.0 - fx) + at(y0, *x1) * fx;
            let bottom = at(y1, *x0) * (1.0 - fx) + at(y1, *x1) * fx;
            if let Some(s) = out.get_mut(y * ow + x) {
                *s = top * (1.0 - fy) + bottom * fy;
            }
        }
    }
    out
}

/// Where tests and examples find the official files: `$FILMCRAFT_SAM2_DIR`, else the first
/// `models/sam2/` in an ancestor of this crate (so a git worktree finds the main checkout's).
#[doc(hidden)]
pub mod tests_support {
    use std::path::PathBuf;

    use super::Sam2Size;

    pub fn dir() -> Option<PathBuf> {
        if let Some(d) = std::env::var_os("FILMCRAFT_SAM2_DIR") {
            return Some(PathBuf::from(d));
        }
        let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        here.ancestors().map(|a| a.join("models/sam2")).find(|p| p.is_dir())
    }

    /// (checkpoint, config) when both are installed, else `None` (and a note on stderr).
    pub fn files(size: Sam2Size) -> Option<(PathBuf, PathBuf)> {
        let (pt, yaml) = size.files();
        let d = dir()?;
        let (pt, yaml) = (d.join(pt), d.join(yaml));
        if pt.is_file() && yaml.is_file() {
            Some((pt, yaml))
        } else {
            eprintln!("skipped: {} not installed", pt.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_and_rope_match_the_reference_formulas() {
        let p = sine_position(2, 3, 4);
        assert_eq!(p.len(), 2 * 3 * 4);
        // token (y=1, x=2): y = 2/2·2π, x = 3/3·2π; channels sin(y), cos(y), sin(x), cos(x)
        let t = &p[(3 + 2) * 4..(3 + 2) * 4 + 4];
        let tau = std::f32::consts::TAU;
        assert!((t[0] - tau.sin()).abs() < 1e-5 && (t[1] - tau.cos()).abs() < 1e-5);
        let s = sine_1d(3.0, 4);
        assert!((s[0] - 3f32.sin()).abs() < 1e-6 && (s[2] - 3f32.cos()).abs() < 1e-6);
        // dim_t = T^(2·(i // 2) / half): pairs share a frequency
        assert!((s[1] - 3f32.sin()).abs() < 1e-6);
        let (c, sn) = rope_tables(4, 8, 10_000.0);
        assert_eq!(c.len(), 16 * 4);
        // token 6 = (x 2, y 1): pair 0 turns by 2·1, pair 2 by 1·1
        assert!((c[6 * 4] - 2f32.cos()).abs() < 1e-6 && (sn[6 * 4 + 2] - 1f32.sin()).abs() < 1e-6);
        let mut x = vec![1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 2.0, 0.0];
        apply_rope(&mut x, 1, 8, &c[6 * 4..], &sn[6 * 4..], 1);
        assert!((x[0] - 2f32.cos()).abs() < 1e-6 && (x[1] - 2f32.sin()).abs() < 1e-6);
    }

    #[test]
    fn bicubic_keeps_constants_and_hits_torch_values() {
        let src = vec![3.5f32; 2 * 7 * 7];
        assert!(bicubic_chw(&src, 2, 7, 7, 256, 256).iter().all(|v| (v - 3.5).abs() < 1e-5));
        // 1-D step 0, 1 upsampled ×2 with A = −0.75 and edge-clamped taps (worked by hand from
        // PyTorch's upsample_bicubic2d: source index (o + 0.5) / 2 − 0.5, not clamped)
        let r = bicubic_chw(&[0.0, 1.0], 1, 1, 2, 1, 4);
        for (a, b) in r.iter().zip([-27.0f32, 58.0, 198.0, 283.0].map(|v| v / 256.0)) {
            assert!((a - b).abs() < 1e-6, "{r:?}");
        }
        let b = bilinear_plane(&[0.0, 1.0, 2.0, 3.0], 2, 2, 4, 4);
        assert!((b[0] - 0.0).abs() < 1e-6 && (b[1] - 0.25).abs() < 1e-6 && (b[15] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn input_is_squashed_to_the_square_and_normalised() {
        let img = RgbImage::new(4, 2, vec![255; 4 * 2 * 3]).unwrap();
        let v = prepare_input(&img);
        assert_eq!(v.len(), INPUT_LEN);
        let want = (1.0 - 0.485) / 0.229;
        assert!((v[0] - want).abs() < 1e-5 && (v[IMAGE_SIZE * IMAGE_SIZE - 1] - want).abs() < 1e-5);
        let big = RgbImage::new(2048, 1100, vec![0; 2048 * 1100 * 3]).unwrap();
        assert!((prepare_input(&big)[5] + 0.485 / 0.229).abs() < 1e-5);
    }
}
