//! Bitmap mattes (fork G3.2, Magic Mask): an 8-bit coverage image that limits an effect, the
//! bitmap counterpart of the polygon masks in [`crate::mask`].
//!
//! A [`BitmapMatte`] may be any size (a matte segmented on the preview image drives the full-res
//! render): it is stretched over the whole working image and sampled bilinearly at each pixel
//! centre, edge texels clamped, as
//!
//! ```text
//! u   = clamp((x + 0.5) · (mw / w) − 0.5, 0, mw − 1)      (v likewise along y)
//! cov = bilinear(alpha, u, v) / 255
//! ```
//!
//! in `f32`; `filmcraft_gpu`'s `matte_mix` (`fx.wgsl`) runs the same arithmetic. A matte whose
//! data does not match its size is invalid and covers nothing.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};

use rayon::prelude::*;

use crate::image::Image;

/// The largest matte side accepted.
pub const MAX_MATTE_SIDE: u32 = 16384;

/// The largest matte side the GPU takes (wgpu's guaranteed 2D texture size); larger mattes
/// render on the CPU.
pub const GPU_MATTE_MAX: u32 = 8192;

/// An 8-bit matte: `alpha` row-major, `width × height` values, 0 (outside) … 255 (inside).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitmapMatte {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl BitmapMatte {
    /// A matte of `width × height` (each 1…=[`MAX_MATTE_SIDE`]); None when `alpha` has another
    /// length or a side is out of range.
    pub fn new(width: u32, height: u32, alpha: Vec<u8>) -> Option<Self> {
        let m = BitmapMatte { width, height, alpha };
        m.is_valid().then_some(m)
    }

    /// Sides in range and `alpha` exactly `width × height` long.
    pub fn is_valid(&self) -> bool {
        (1..=MAX_MATTE_SIDE).contains(&self.width)
            && (1..=MAX_MATTE_SIDE).contains(&self.height)
            && (self.width as usize).checked_mul(self.height as usize) == Some(self.alpha.len())
    }
}

/// Per output coordinate along one axis: (left texel, right texel, weight of the right one).
fn axis_taps(n: usize, m: usize) -> Vec<(usize, usize, f32)> {
    let last = m.saturating_sub(1);
    let scale = m as f32 / n.max(1) as f32;
    (0..n)
        .map(|i| {
            let c = ((i as f32 + 0.5) * scale - 0.5).clamp(0.0, last as f32);
            let a = (c.floor() as usize).min(last);
            let b = a.saturating_add(1).min(last);
            (a, b, c - a as f32)
        })
        .collect()
}

/// Coverage (0…1) of `matte` stretched over a `w`×`h` image, or None when the matte is invalid.
pub fn coverage(matte: &BitmapMatte, w: usize, h: usize) -> Option<Vec<f32>> {
    if !matte.is_valid() {
        return None;
    }
    let (mw, mh) = (matte.width as usize, matte.height as usize);
    let (xs, ys) = (axis_taps(w, mw), axis_taps(h, mh));
    let at = |x: usize, y: usize| y.checked_mul(mw).and_then(|r| r.checked_add(x)).and_then(|i| matte.alpha.get(i)).map_or(0.0, |&v| f32::from(v) / 255.0);
    let mut cov = vec![0.0f32; w.saturating_mul(h)];
    if w == 0 {
        return Some(cov);
    }
    cov.par_chunks_mut(w).zip(ys.par_iter()).for_each(|(row, &(y0, y1, fy))| {
        for (c, &(x0, x1, fx)) in row.iter_mut().zip(&xs) {
            let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
            let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
            *c = top + (bot - top) * fy;
        }
    });
    Some(cov)
}

/// Run `apply` on `img` limited to `matte`: `lerp(before, after, coverage)` per premultiplied
/// channel. An invalid matte leaves `img` untouched (`apply` is not run).
pub fn apply_masked(img: &mut Image, matte: &BitmapMatte, apply: impl FnOnce(&mut Image)) {
    let Some(cov) = coverage(matte, img.w, img.h) else { return };
    let original = img.clone();
    apply(img);
    crate::mask::mix(img, &original, &cov);
}

// ---------------------------------------------------------------------------------------------
// Matte sources (fork G3.2): where the renderer gets a node's Magic Mask matte
// ---------------------------------------------------------------------------------------------
//
// [`crate::RenderOptions`] is `Copy` and built in many places, so it carries only a
// [`MatteToken`]: a number naming a [`MatteSource`] in a process-wide registry. The engine
// registers its session's matte cache once ([`register`]) and sets the token on the options of
// the renders that should show Magic Mask mattes (Program monitor, render cache, export). The
// registry holds only a `Weak`: a dropped session's token simply finds nothing (no limit).
// Provisional design: a later pass may replace it with a source handed through the render call.

/// Where the renderer gets Magic Mask mattes.
pub trait MatteSource: Send + Sync {
    /// The refined matte of grade node `node` (one with a Magic Mask) of track item `item` at
    /// clip time `source_t` (`TrackItem::effect_time_at`, the time grade keyframes use), in
    /// **sequence-frame** coordinates: the matte is stretched over the whole sequence frame (it
    /// was segmented on the clip as placed in the frame). None: not tracked for the
    /// current strokes (the node then renders without the limit).
    fn matte(
        &self,
        project: &filmcraft_project::Project,
        item: &filmcraft_project::TrackItem,
        node: &filmcraft_project::grade::GradeNode,
        source_t: filmcraft_time::Tick,
    ) -> Option<Arc<BitmapMatte>>;
}

/// A registered [`MatteSource`] (0, the default: none).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MatteToken(pub u64);

impl MatteToken {
    /// No matte source.
    pub const NONE: MatteToken = MatteToken(0);
    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

type Registry = BTreeMap<u64, Weak<dyn MatteSource>>;

static REGISTRY: RwLock<Registry> = RwLock::new(BTreeMap::new());
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Register `source` (dead registrations are pruned here). The token stays valid while the
/// source is alive.
pub fn register(source: Weak<dyn MatteSource>) -> MatteToken {
    let id = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed).max(1);
    let mut g = REGISTRY.write().unwrap_or_else(PoisonError::into_inner);
    g.retain(|_, w| w.strong_count() > 0);
    g.insert(id, source);
    MatteToken(id)
}

/// A registration that owns its source: the token works while this lives, and dropping it
/// unregisters.
pub struct Registration {
    token: MatteToken,
    _source: Arc<dyn MatteSource>,
}

impl Registration {
    /// Register `source`, kept alive by the handle.
    pub fn new(source: Arc<dyn MatteSource>) -> Self {
        let token = register(Arc::downgrade(&source));
        Self { token, _source: source }
    }

    pub fn token(&self) -> MatteToken {
        self.token
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        unregister(self.token);
    }
}

impl std::fmt::Debug for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registration").field("token", &self.token).finish()
    }
}

/// Forget a registration (a no-op for an unknown token).
pub fn unregister(token: MatteToken) {
    REGISTRY.write().unwrap_or_else(PoisonError::into_inner).remove(&token.0);
}

/// The live source of `token`.
pub fn source(token: MatteToken) -> Option<Arc<dyn MatteSource>> {
    if token.is_none() {
        return None;
    }
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner).get(&token.0)?.upgrade()
}

/// The longest side of a matte warped into a layer (never more than the matte's own long side:
/// the mix upsamples it anyway).
const MAX_WARPED_SIDE: usize = 2048;

/// Warped mattes kept for reuse (a paused frame, the GPU plan and the CPU path of one frame).
const WARP_MEMO: usize = 16;

/// (source matte, its key) → warped matte; the source `Arc` is kept so its address stays unique.
type WarpKey = ([u64; 6], (u64, u64), usize, usize);
static WARPS: Mutex<Vec<(Arc<BitmapMatte>, WarpKey, Arc<BitmapMatte>)>> = Mutex::new(Vec::new());

/// [`warp_to_layer`], memoized by (matte, affine, sequence size, layer size).
fn warp_memo(matte: &Arc<BitmapMatte>, seq: (f64, f64), m: &filmcraft_geom::Affine, lw: usize, lh: usize) -> Option<Arc<BitmapMatte>> {
    let key: WarpKey = ([m.a, m.b, m.c, m.d, m.e, m.f].map(f64::to_bits), (seq.0.to_bits(), seq.1.to_bits()), lw, lh);
    {
        let g = WARPS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, _, out)) = g.iter().find(|(src, k, _)| Arc::ptr_eq(src, matte) && *k == key) {
            return Some(out.clone());
        }
    }
    let out = warp_to_layer(matte, seq, m, lw, lh)?;
    let mut g = WARPS.lock().unwrap_or_else(PoisonError::into_inner);
    if g.len() >= WARP_MEMO {
        g.remove(0);
    }
    g.push((matte.clone(), key, out.clone()));
    Some(out)
}

/// The matte of a Magic Mask node with nothing tracked for the frame: it covers nothing (the
/// node changes nothing there).
fn empty_matte() -> Arc<BitmapMatte> {
    static EMPTY: std::sync::OnceLock<Arc<BitmapMatte>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Arc::new(BitmapMatte { width: 1, height: 1, alpha: vec![0] })).clone()
}

/// `matte` (stretched over a `seq_w × seq_h` sequence frame) as a matte stretched over a
/// `lw × lh` layer whose pixel (x, y) lands at `layer_to_seq · (x, y)` in sequence pixels;
/// outside the matte covers nothing. The layer's own aspect is kept, at most the matte's own
/// long side (and [`MAX_WARPED_SIDE`]). A layer that fills the frame exactly gets `matte` back.
pub fn warp_to_layer(matte: &Arc<BitmapMatte>, seq: (f64, f64), layer_to_seq: &filmcraft_geom::Affine, lw: usize, lh: usize) -> Option<Arc<BitmapMatte>> {
    let (sw, sh) = seq;
    if !matte.is_valid() || lw == 0 || lh == 0 || !(sw > 0.0 && sh > 0.0) {
        return None;
    }
    let m = layer_to_seq;
    let (fx, fy) = (lw as f64 / sw, lh as f64 / sh);
    let near = |v: f64, want: f64| (v - want).abs() <= 1e-6 * want.abs().max(1.0);
    if near(m.a * fx, 1.0) && near(m.d * fy, 1.0) && m.b.abs() < 1e-9 && m.c.abs() < 1e-9 && m.e.abs() < 1e-6 && m.f.abs() < 1e-6 {
        return Some(matte.clone());
    }
    let long = lw.max(lh);
    let cap = MAX_WARPED_SIDE.min((matte.width.max(matte.height) as usize).max(1));
    let k = if long > cap { cap as f64 / long as f64 } else { 1.0 };
    let (ow, oh) = (((lw as f64 * k).round() as usize).max(1), ((lh as f64 * k).round() as usize).max(1));
    // output pixel → layer pixel → sequence pixel → matte texel
    let (sx, sy) = (lw as f64 / ow as f64, lh as f64 / oh as f64);
    let (mw, mh) = (matte.width as usize, matte.height as usize);
    let (tx, ty) = (mw as f64 / sw, mh as f64 / sh);
    let at = |x: usize, y: usize| y.checked_mul(mw).and_then(|r| r.checked_add(x)).and_then(|i| matte.alpha.get(i)).map_or(0.0, |&v| f64::from(v));
    let mut alpha = vec![0u8; ow.checked_mul(oh)?];
    alpha.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        for (ox, out) in row.iter_mut().enumerate() {
            let (lx, ly) = ((ox as f64 + 0.5) * sx, (oy as f64 + 0.5) * sy);
            let (qx, qy) = (m.a * lx + m.c * ly + m.e, m.b * lx + m.d * ly + m.f);
            if !(qx >= 0.0 && qy >= 0.0 && qx < sw && qy < sh) {
                continue;
            }
            let u = (qx * tx - 0.5).clamp(0.0, (mw - 1) as f64);
            let v = (qy * ty - 0.5).clamp(0.0, (mh - 1) as f64);
            let (x0, y0) = (u.floor() as usize, v.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(mw - 1), (y0 + 1).min(mh - 1));
            let (wx, wy) = (u - x0 as f64, v - y0 as f64);
            let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * wx;
            let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * wx;
            *out = (top + (bot - top) * wy).round().clamp(0.0, 255.0) as u8;
        }
    });
    BitmapMatte::new(ow as u32, oh as u32, alpha).map(Arc::new)
}

/// The Magic Mask mattes of `item`'s grade nodes at clip time `mt`, from the source of
/// `token`, each warped into the item's `lw × lh` working layer (`layer_to_seq`: layer pixel →
/// sequence pixel). A Magic Mask node without a matte for the frame (not tracked, tracked for
/// other strokes or another picture, no source at all) gets an empty matte: it changes nothing
/// rather than the whole picture. None when no node has a Magic Mask. Serial grades and node
/// graphs (fork C2) both use these.
#[allow(clippy::too_many_arguments)]
pub fn layer_mattes(
    project: &filmcraft_project::Project,
    seq: &filmcraft_project::Sequence,
    item: &filmcraft_project::TrackItem,
    mt: filmcraft_time::Tick,
    token: MatteToken,
    layer_to_seq: &filmcraft_geom::Affine,
    lw: usize,
    lh: usize,
) -> Option<crate::grade::NodeMattes> {
    let g = item.grade.as_ref().filter(|g| g.nodes.iter().any(|n| n.magic_mask.is_some()))?;
    let src = source(token);
    let size = (f64::from(seq.settings.width), f64::from(seq.settings.height));
    let mut out = BTreeMap::new();
    for n in g.nodes.iter().filter(|n| n.magic_mask.is_some()) {
        let m = src.as_ref().and_then(|s| s.matte(project, item, n, mt)).and_then(|m| warp_memo(&m, size, layer_to_seq, lw, lh));
        out.insert(n.id, m.unwrap_or_else(empty_matte));
    }
    Some(crate::grade::NodeMattes(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpufx::FxOp;
    use std::sync::Arc;

    fn picture(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for (i, p) in img.px.chunks_mut(4).enumerate() {
            let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
            p.copy_from_slice(&[x * 0.9, y * 0.8, 0.3 + 0.2 * x, 1.0]);
        }
        img
    }

    fn run(ops: &[FxOp], img: &Image) -> Image {
        let mut out = img.clone();
        for op in ops {
            op.apply(&mut out);
        }
        out
    }

    fn worst(a: &Image, b: &Image) -> f32 {
        a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
    }

    fn masked(m: BitmapMatte) -> FxOp {
        FxOp::MaskedBy { ops: vec![FxOp::BlackWhite], matte: Arc::new(m) }
    }

    #[test]
    fn the_constructor_validates_sizes() {
        assert!(BitmapMatte::new(4, 2, vec![0; 8]).is_some());
        assert!(BitmapMatte::new(4, 2, vec![0; 7]).is_none());
        assert!(BitmapMatte::new(0, 0, vec![]).is_none());
        assert!(BitmapMatte::new(MAX_MATTE_SIDE + 1, 1, vec![0; MAX_MATTE_SIDE as usize + 1]).is_none());
        assert!(!BitmapMatte { width: u32::MAX, height: u32::MAX, alpha: vec![] }.is_valid());
    }

    #[test]
    fn full_and_empty_mattes_are_the_effect_and_the_input() {
        let img = picture(40, 24);
        let all = run(&[FxOp::BlackWhite], &img);
        let full = run(&[masked(BitmapMatte::new(5, 3, vec![255; 15]).unwrap())], &img);
        assert!(worst(&full, &all) <= 1e-6, "{}", worst(&full, &all));
        let empty = run(&[masked(BitmapMatte::new(5, 3, vec![0; 15]).unwrap())], &img);
        assert_eq!(empty, img);
    }

    #[test]
    fn a_half_matte_splits_the_frame() {
        let (w, h) = (40, 24);
        let img = picture(w, h);
        let all = run(&[FxOp::BlackWhite], &img);
        // left half inside, at the same size as the frame (no interpolation across the split)
        let alpha: Vec<u8> = (0..w * h).map(|i| if i % w < w / 2 { 255 } else { 0 }).collect();
        let out = run(&[masked(BitmapMatte::new(w as u32, h as u32, alpha).unwrap())], &img);
        for (i, (o, (a, b))) in out.px.chunks(4).zip(all.px.chunks(4).zip(img.px.chunks(4))).enumerate() {
            let want = if i % w < w / 2 { a } else { b };
            assert!(o.iter().zip(want).all(|(x, y)| (x - y).abs() <= 1e-6), "pixel {i}");
        }
    }

    #[test]
    fn a_smaller_matte_is_stretched_bilinearly() {
        // a 2×1 matte (0, 255) over 8 pixels: the edge texels hold, the middle ramps
        let m = BitmapMatte::new(2, 1, vec![0, 255]).unwrap();
        let cov = coverage(&m, 8, 3).unwrap();
        let row: Vec<f32> = cov.get(..8).unwrap().to_vec();
        let want = [0.0, 0.0, 0.125, 0.375, 0.625, 0.875, 1.0, 1.0];
        assert!(row.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-6), "{row:?}");
        assert_eq!(cov.get(8..16), cov.get(..8), "rows repeat");
        // a larger matte is sampled down the same way (a 4× matte of a checker averages per pair)
        let big = BitmapMatte::new(16, 2, (0..32).map(|i| if i % 16 < 8 { 255 } else { 0 }).collect()).unwrap();
        let c = coverage(&big, 4, 1).unwrap();
        assert_eq!(c, vec![1.0, 1.0, 0.0, 0.0]);
        // and the effect follows it
        let img = picture(8, 3);
        let out = run(&[masked(m)], &img);
        let all = run(&[FxOp::BlackWhite], &img);
        let mid = 4 * 3;
        let want = img.px[mid] + (all.px[mid] - img.px[mid]) * 0.375;
        assert!((out.px[mid] - want).abs() < 1e-6);
    }

    #[test]
    fn an_invalid_matte_is_a_no_op() {
        let img = picture(16, 9);
        for bad in [
            BitmapMatte { width: 4, height: 4, alpha: vec![255; 15] },
            BitmapMatte { width: 0, height: 4, alpha: vec![] },
            BitmapMatte { width: u32::MAX, height: 2, alpha: vec![255; 8] },
        ] {
            assert!(coverage(&bad, 16, 9).is_none());
            assert_eq!(run(&[masked(bad)], &img), img);
        }
        // empty images are fine too
        let m = BitmapMatte::new(1, 1, vec![255]).unwrap();
        assert_eq!(coverage(&m, 0, 5), Some(vec![]));
        assert_eq!(coverage(&m, 5, 0), Some(vec![]));
    }

    #[test]
    fn gpu_ok_follows_the_matte_and_the_inner_ops() {
        assert!(masked(BitmapMatte::new(2, 2, vec![0; 4]).unwrap()).gpu_ok());
        assert!(!masked(BitmapMatte { width: 2, height: 2, alpha: vec![] }).gpu_ok());
        let wide = BitmapMatte::new(GPU_MATTE_MAX + 1, 1, vec![0; GPU_MATTE_MAX as usize + 1]).unwrap();
        assert!(!masked(wide).gpu_ok());
        let m = Arc::new(BitmapMatte::new(1, 1, vec![9]).unwrap());
        let nested = FxOp::MaskedBy { ops: vec![FxOp::MaskedBy { ops: vec![], matte: m.clone() }], matte: m.clone() };
        assert!(!nested.gpu_ok());
        assert!(!FxOp::MaskedBy { ops: vec![FxOp::Gamma { g: f32::NAN }], matte: m }.gpu_ok());
    }
}
