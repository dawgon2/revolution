//! 3D qualifier (fork GRADE-PLAN G4.3), modelled on Resolve's 3D qualifier: colours picked by
//! strokes on the picture define a soft volume in colour space; the key is how close a pixel's
//! colour is to that volume ([`ColorOp::Key3d`](crate::ops::ColorOp::Key3d)). A despill
//! ([`ColorOp::Despill`](crate::ops::ColorOp::Despill)) pulls the picked colour's chroma out of
//! the pixels it is applied to.
//!
//! **Colour space.** The key runs on the grading signal (what the node sees, e.g. DaVinci
//! Intermediate in a DaVinci Wide Gamut project), brought into 0…1 like the HSL qualifier
//! ([`crate::ops::to_unit`]: clamped, or with HDR scaled by its largest component), then turned
//! into an opponent space ([`opponent`]): an orthonormal rotation of RGB with the grey axis
//! first, so distances are RGB distances, and that grey (lightness) axis weighted by
//! [`LUMA_WEIGHT`]. Lightness counts half as much as colour: a shaded and a lit part of the same
//! red jacket stay close, a red and an orange of the same lightness don't.
//!
//! **Volume.** The include and exclude volumes are each a union of spheres (cluster centres in
//! the opponent space with a radius: how far the picked samples spread). For a pixel `p`, the
//! distance to a volume is `min_i max(|p − cᵢ| − rᵢ, 0)` (0 inside), and its soft membership
//! `m(d) = 1 − smoothstep(0, soft, d)`. The key is `m_include · (1 − m_exclude)`; no include
//! sphere keys nothing, no exclude sphere excludes nothing. Non-finite pixels key 0.
//!
//! **Despill.** `rgb' = rgb − amount · max(rgb · v, 0) · v` with `v` the unit chroma direction of
//! the picked colour (perpendicular to grey, [`despill_dir`]): the part of the pixel's chroma
//! that points towards the picked colour is reduced by `amount` (1: removed), lightness and the
//! perpendicular chroma kept. In a grade node it is part of the node's correction, so it acts
//! inside the node's matte (with the qualifier inverted: outside the picked colour, which is
//! where a green screen's spill on the subject is). Non-finite pixels pass unchanged.
//!
//! The GPU (`colorops.wgsl`, `CO_KEY3D`) does the same arithmetic.

/// Weight of the lightness axis in the opponent space.
pub const LUMA_WEIGHT: f32 = 0.5;
/// Most spheres on each side (include, exclude) a key keeps.
pub const MAX_SPHERES: usize = 64;
/// Narrowest softness (opponent-space units).
pub const MIN_SOFT: f32 = 1e-3;

const INV_SQRT2: f32 = std::f32::consts::FRAC_1_SQRT_2;
/// 1/√6 and 1/√3.
const INV_SQRT6: f32 = 0.408_248_3;
const INV_SQRT3: f32 = 0.577_350_26;

/// The opponent coordinates of a 0…1 colour: `[LUMA_WEIGHT · (r + g + b)/√3, (r − g)/√2,
/// (r + g − 2b)/√6]`.
#[inline]
pub fn opponent(u: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = u;
    [LUMA_WEIGHT * (r + g + b) * INV_SQRT3, (r - g) * INV_SQRT2, (r + g - 2.0 * b) * INV_SQRT6]
}

/// The RGB direction (unit, perpendicular to grey) of the chroma `[a, b]` of the opponent space;
/// None when the chroma is too small to have a direction (a grey) or not finite.
pub fn despill_dir(ab: [f32; 2]) -> Option<[f32; 3]> {
    let len = (ab[0] * ab[0] + ab[1] * ab[1]).sqrt();
    if !len.is_finite() || len < 1e-3 {
        return None;
    }
    let (a, b) = (ab[0] / len, ab[1] / len);
    // a·e_a + b·e_b with e_a = (1, −1, 0)/√2, e_b = (1, 1, −2)/√6 (orthonormal: the sum is unit)
    Some([a * INV_SQRT2 + b * INV_SQRT6, -a * INV_SQRT2 + b * INV_SQRT6, -2.0 * b * INV_SQRT6])
}

/// The despill of one pixel (module docs).
#[inline]
pub fn despill(c: [f32; 3], dir: [f32; 3], amount: f32) -> [f32; 3] {
    let k = c[0] * dir[0] + c[1] * dir[1] + c[2] * dir[2];
    // (NaN / ∞: the pixel passes unchanged)
    if !k.is_finite() || k <= 0.0 {
        return c;
    }
    let s = amount * k;
    [c[0] - s * dir[0], c[1] - s * dir[1], c[2] - s * dir[2]]
}

/// A 3D key: include and exclude spheres in the opponent space and the softness.
#[derive(Clone, Debug, PartialEq)]
pub struct Key3d {
    /// `[x, y, z, radius]`, include spheres first.
    spheres: Vec<[f32; 4]>,
    include: usize,
    soft: f32,
    hdr: bool,
}

impl Key3d {
    /// A key from include / exclude spheres (`[x, y, z, radius]`). Non-finite spheres are dropped,
    /// negative radii read as 0, each side keeps at most [`MAX_SPHERES`]; the softness is at least
    /// [`MIN_SOFT`] (NaN: [`MIN_SOFT`], +∞: 1).
    pub fn new(include: &[[f32; 4]], exclude: &[[f32; 4]], soft: f32, hdr: bool) -> Self {
        let soft = if soft.is_nan() { MIN_SOFT } else { soft.clamp(MIN_SOFT, 1.0) };
        let clean = |s: &[[f32; 4]]| -> Vec<[f32; 4]> {
            s.iter().filter(|v| v.iter().all(|x| x.is_finite())).take(MAX_SPHERES).map(|v| [v[0], v[1], v[2], v[3].max(0.0)]).collect()
        };
        let mut spheres = clean(include);
        let n = spheres.len();
        spheres.extend(clean(exclude));
        Self { spheres, include: n, soft, hdr }
    }

    pub fn include(&self) -> &[[f32; 4]] {
        self.spheres.get(..self.include).unwrap_or_default()
    }

    pub fn exclude(&self) -> &[[f32; 4]] {
        self.spheres.get(self.include..).unwrap_or_default()
    }

    pub fn soft(&self) -> f32 {
        self.soft
    }

    pub fn hdr(&self) -> bool {
        self.hdr
    }

    /// The key of a pixel of the grading signal (module docs).
    pub fn matte(&self, c: [f32; 3]) -> f32 {
        if !c.iter().all(|v| v.is_finite()) {
            return 0.0;
        }
        let p = opponent(crate::ops::to_unit(c, self.hdr));
        let inc = membership(self.include(), p, self.soft);
        if inc <= 0.0 {
            return 0.0;
        }
        inc * (1.0 - membership(self.exclude(), p, self.soft))
    }
}

/// Soft membership of `p` in a union of spheres (0 for none).
fn membership(spheres: &[[f32; 4]], p: [f32; 3], soft: f32) -> f32 {
    let mut d = f32::INFINITY;
    for s in spheres {
        let (dx, dy, dz) = (p[0] - s[0], p[1] - s[1], p[2] - s[2]);
        d = d.min(((dx * dx + dy * dy + dz * dz).sqrt() - s[3]).max(0.0));
    }
    if d == f32::INFINITY {
        return 0.0;
    }
    let t = (d / soft).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opponent_is_a_weighted_rotation() {
        let g = opponent([0.5, 0.5, 0.5]);
        assert!(g[1].abs() < 1e-7 && g[2].abs() < 1e-7);
        // distances of equal-lightness colours are RGB distances
        let (a, b) = (opponent([0.6, 0.4, 0.5]), opponent([0.4, 0.6, 0.5]));
        let d = ((a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        assert!((d - (0.08f32).sqrt()).abs() < 1e-6, "{d}");
    }

    #[test]
    fn key_is_one_inside_soft_at_the_edge_and_minus_exclude() {
        let red = opponent([0.8, 0.2, 0.2]);
        let k = Key3d::new(&[[red[0], red[1], red[2], 0.05]], &[], 0.1, false);
        assert_eq!(k.matte([0.8, 0.2, 0.2]), 1.0);
        let far = k.matte([0.2, 0.2, 0.8]);
        assert_eq!(far, 0.0);
        // halfway through the softness: ½
        let dir = [1.0f32, -1.0, 0.0].map(|v| v * INV_SQRT2);
        let off = 0.05 + 0.05;
        let c = [0.8 + off * dir[0], 0.2 + off * dir[1], 0.2];
        assert!((k.matte(c) - 0.5).abs() < 1e-3, "{}", k.matte(c));
        // an exclude sphere at the same place removes it
        let k = Key3d::new(&[[red[0], red[1], red[2], 0.05]], &[[red[0], red[1], red[2], 0.01]], 0.1, false);
        assert_eq!(k.matte([0.8, 0.2, 0.2]), 0.0);
        // no include: nothing
        assert_eq!(Key3d::new(&[], &[], 0.1, false).matte([0.8, 0.2, 0.2]), 0.0);
    }

    #[test]
    fn hostile_keys_and_pixels() {
        assert_eq!(Key3d::new(&[], &[], f32::NAN, false).soft(), MIN_SOFT);
        assert_eq!(Key3d::new(&[], &[], f32::INFINITY, false).soft(), 1.0);
        let many = vec![[0.1, 0.0, 0.0, -1.0]; 500];
        let k = Key3d::new(&many, &[[f32::NAN, 0.0, 0.0, 0.1], [0.0, 0.0, 0.0, f32::INFINITY]], 0.0, true);
        assert_eq!(k.include().len(), MAX_SPHERES);
        assert!(k.exclude().is_empty());
        assert_eq!(k.soft(), MIN_SOFT);
        assert!(k.include().iter().all(|s| s[3] == 0.0));
        for c in [[f32::NAN, 0.0, 0.0], [f32::INFINITY, 1.0, 1.0], [0.0, f32::NEG_INFINITY, 0.0]] {
            assert_eq!(k.matte(c), 0.0);
            let d = despill(c, [INV_SQRT2, -INV_SQRT2, 0.0], 1.0);
            assert!(d.iter().zip(c).all(|(a, b)| a.to_bits() == b.to_bits()), "passes unchanged");
        }
    }

    #[test]
    fn despill_removes_only_the_picked_chroma() {
        let green = opponent([0.2, 0.8, 0.2]);
        let dir = despill_dir([green[1], green[2]]).unwrap();
        assert!((dir.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(dir.iter().sum::<f32>().abs() < 1e-6, "perpendicular to grey");
        let out = despill([0.2, 0.8, 0.2], dir, 1.0);
        assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6, "neutral: {out:?}");
        assert!(((out[0] + out[1] + out[2]) - 1.2).abs() < 1e-5, "lightness kept");
        // magenta (the opposite chroma) is left alone
        assert_eq!(despill([0.8, 0.2, 0.8], dir, 1.0), [0.8, 0.2, 0.8]);
        assert!(despill_dir([0.0, 0.0]).is_none() && despill_dir([f32::NAN, 1.0]).is_none());
    }
}
