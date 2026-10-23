//! Colour warper (fork GRADE-PLAN G4.1): a 2D displacement field over a colour plane, compiled
//! from a mesh of control points the colourist drags ([`compile`]) and applied per pixel as
//! [`ColorOp::Warp2D`](crate::ops::ColorOp::Warp2D).
//!
//! **Colour model.** The warp runs on the grading signal (whatever the node sees, e.g. DaVinci
//! Intermediate in a DaVinci Wide Gamut project) in HSV: `v = max(r, g, b)`,
//! `s = (v − min(r, g, b)) / v`, the hue as in [`crate::rgb_to_hsl`]. Non-finite pixels pass
//! unchanged. HSV keeps `v` when hue
//! and saturation move, works above 1 (HDR) and is continuous for the negative components a wide
//! gamut produces (`s` then exceeds 1; it is clamped only for the lookup). Pixels with
//! `v ≤ 1e-6` (black, and NaN) pass unchanged. The luma is `weights · rgb`.
//!
//! **Planes.**
//! - [`WarpSpace::HueSat`]: the field is over (hue, saturation), hue wrapping. A point displaced
//!   by (Δh, Δs) turns the colours under it by Δh and adds Δs saturation. With
//!   `preserve_luma` the result is scaled back to the input's luma.
//! - [`WarpSpace::ChromaLuma`]: the field is over (saturation, luma) for every hue. A point
//!   displaced by (Δs, Δl) adds Δs saturation (hue kept) and moves the luma by Δl (scaling the
//!   colour, which keeps its hue and saturation). Pulling saturated highlights down is a
//!   "colour density" look.
//!
//! **Lookup.** The field is a grid of `cols × rows` (Δu, Δv) samples, bilinearly interpolated.
//! Hue-sat: column `i` at hue `i / cols` (the last column wraps to the first), row `j` at
//! saturation `j / (rows − 1)`. Chroma-luma: column `i` at saturation `i / (cols − 1)`, row `j`
//! at luma `j / (rows − 1)`; coordinates outside 0…1 are clamped. Where the interpolated
//! displacement is exactly zero the pixel passes unchanged (bit exact), so colours away from the
//! moved points are untouched. The GPU (`colorops.wgsl` `warp2d`) does the same arithmetic.
//!
//! **Mesh → field.** [`compile`] takes a coarse mesh (the points the UI shows), gives every
//! point a displacement (moved points their own, pinned points theirs or none, free points a
//! smooth falloff of the moved points within `range` grid steps; range 0: neighbours stay put),
//! scales it by the strength and upsamples it [`UPSAMPLE`]× with Catmull-Rom splines (smooth
//! through every point, exact at the points), each sample clamped to the range of its cell's four
//! points (no overshoot: a cell whose corners don't move stays exactly zero).

use std::sync::Arc;

/// Field samples between two mesh points along each axis.
pub const UPSAMPLE: u32 = 8;
/// Largest field side (samples).
pub const MAX_FIELD_SIDE: u32 = 1024;
/// Largest mesh side a compile accepts (points).
pub const MAX_MESH_SIDE: u32 = 64;
/// Largest falloff range (mesh steps).
pub const MAX_RANGE: f32 = 4.0;
/// Below this `max(r, g, b)` a pixel passes unchanged.
const DARK: f32 = 1e-6;

/// Which colour plane a warp acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WarpSpace {
    /// Hue (wrapping) × saturation.
    HueSat,
    /// Saturation × luma, for every hue.
    ChromaLuma,
}

impl WarpSpace {
    /// The GPU's code for it.
    pub fn code(self) -> u32 {
        match self {
            WarpSpace::HueSat => 0,
            WarpSpace::ChromaLuma => 1,
        }
    }
}

/// A compiled warp: the displacement field and how it applies.
#[derive(Clone, Debug, PartialEq)]
pub struct Warp2D {
    space: WarpSpace,
    cols: u32,
    rows: u32,
    field: Arc<[f32]>,
    preserve_luma: bool,
    weights: [f32; 3],
}

impl Warp2D {
    /// A warp over a `cols × rows` field of `(Δu, Δv)` pairs, row major. None unless both sides
    /// are 2…[`MAX_FIELD_SIDE`], `field` holds exactly `cols · rows · 2` finite values and the
    /// weights are finite.
    pub fn new(space: WarpSpace, cols: u32, rows: u32, field: Vec<f32>, preserve_luma: bool, weights: [f32; 3]) -> Option<Self> {
        let side = 2..=MAX_FIELD_SIDE;
        if !side.contains(&cols) || !side.contains(&rows) {
            return None;
        }
        let n = (cols as usize).checked_mul(rows as usize)?.checked_mul(2)?;
        if field.len() != n || !field.iter().chain(&weights).all(|v| v.is_finite()) {
            return None;
        }
        Some(Self { space, cols, rows, field: field.into(), preserve_luma, weights })
    }

    pub fn space(&self) -> WarpSpace {
        self.space
    }
    pub fn cols(&self) -> u32 {
        self.cols
    }
    pub fn rows(&self) -> u32 {
        self.rows
    }
    /// `(Δu, Δv)` per sample, row major.
    pub fn field(&self) -> &[f32] {
        &self.field
    }
    pub fn preserve_luma(&self) -> bool {
        self.preserve_luma
    }
    pub fn weights(&self) -> [f32; 3] {
        self.weights
    }

    fn at(&self, i: u32, j: u32) -> [f32; 2] {
        let k = (j as usize * self.cols as usize + i as usize) * 2;
        [self.field.get(k).copied().unwrap_or(0.0), self.field.get(k + 1).copied().unwrap_or(0.0)]
    }

    /// The displacement at plane coordinates (`u`, `v`) (bilinear; hue wraps).
    pub fn lookup(&self, u: f32, v: f32) -> [f32; 2] {
        let (nc, nr) = (self.cols as f32, self.rows as f32);
        let (i0, i1, fx) = match self.space {
            WarpSpace::HueSat => {
                let x = (u - u.floor()) * nc;
                let i = x.floor().clamp(0.0, nc - 1.0);
                let i0 = i as u32;
                (i0, if i0 + 1 >= self.cols { 0 } else { i0 + 1 }, x - i)
            }
            WarpSpace::ChromaLuma => {
                let x = u.clamp(0.0, 1.0) * (nc - 1.0);
                let i = x.floor().clamp(0.0, nc - 2.0);
                (i as u32, i as u32 + 1, x - i)
            }
        };
        let y = v.clamp(0.0, 1.0) * (nr - 1.0);
        let j = y.floor().clamp(0.0, nr - 2.0);
        let fy = y - j;
        let (j0, j1) = (j as u32, j as u32 + 1);
        let (a, b, c, d) = (self.at(i0, j0), self.at(i1, j0), self.at(i0, j1), self.at(i1, j1));
        [0, 1].map(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            top + (bot - top) * fy
        })
    }

    /// Apply to one pixel (module docs).
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let mx = c[0].max(c[1]).max(c[2]);
        if !c.iter().all(|v| v.is_finite()) || mx <= DARK {
            return c;
        }
        let mn = c[0].min(c[1]).min(c[2]);
        let d = mx - mn;
        let s = d / mx;
        let h = hsv_hue(c, mx, d);
        let w = self.weights;
        let y = w[0] * c[0] + w[1] * c[1] + w[2] * c[2];
        let (u, v) = match self.space {
            WarpSpace::HueSat => (h, s.clamp(0.0, 1.0)),
            WarpSpace::ChromaLuma => (s.clamp(0.0, 1.0), y),
        };
        let [du, dv] = self.lookup(u, v);
        if du == 0.0 && dv == 0.0 {
            return c;
        }
        match self.space {
            WarpSpace::HueSat => {
                let h2 = h + du;
                let o = hsv_to_rgb(h2 - h2.floor(), (s + dv).max(0.0), mx);
                if self.preserve_luma { scale_luma(o, y, w) } else { o }
            }
            WarpSpace::ChromaLuma => scale_luma(hsv_to_rgb(h, (s + du).max(0.0), mx), (y + dv).max(0.0), w),
        }
    }
}

/// The HSV hue (0…1) of `c` with `max` and `max − min` given.
fn hsv_hue(c: [f32; 3], mx: f32, d: f32) -> f32 {
    if d < 1e-7 {
        return 0.0;
    }
    let [r, g, b] = c;
    let h = if mx == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h / 6.0
}

/// HSV → RGB (`s` may exceed 1: components then go negative, continuously).
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    [5.0f32, 3.0, 1.0].map(|n| {
        let k0 = n + h * 6.0;
        let k = k0 - 6.0 * (k0 / 6.0).floor();
        v - v * s * k.min(4.0 - k).clamp(0.0, 1.0)
    })
}

/// Below this luma a colour is brought to its target luma by adding grey instead of scaling
/// (near-black wide-gamut colours would otherwise be scaled by huge factors).
const SCALE_FLOOR: f32 = 1e-3;
/// Largest factor a colour is scaled by to reach its target luma.
const MAX_SCALE: f32 = 16.0;

/// `o` brought to luma `t`: scaled (by at most [`MAX_SCALE`]) when `t` ≥ 0 and `o`'s luma is
/// above [`SCALE_FLOOR`], else lifted (or lowered) by grey.
fn scale_luma(o: [f32; 3], t: f32, w: [f32; 3]) -> [f32; 3] {
    let y2 = w[0] * o[0] + w[1] * o[1] + w[2] * o[2];
    if t >= 0.0 && y2 > SCALE_FLOOR {
        let k = (t / y2).clamp(0.0, MAX_SCALE);
        o.map(|q| q * k)
    } else {
        o.map(|q| q + (t - y2))
    }
}

/// A control mesh to compile ([`compile`]).
#[derive(Clone, Debug, PartialEq)]
pub struct MeshSpec<'a> {
    pub space: WarpSpace,
    /// Points across: hue columns (hue-sat) or saturation columns (chroma-luma).
    pub cols: u32,
    /// Points down: saturation rings (hue-sat, the centre not counted) or luma rows.
    pub rows: u32,
    /// Moved points: (index, displacement `[Δu, Δv]` in plane units).
    pub moved: &'a [(u32, [f32; 2])],
    /// Pinned points (by index): they keep their own displacement (none unless moved) whatever
    /// the falloff of their neighbours.
    pub pinned: &'a [u32],
    /// Falloff of each move into the free points around it, in mesh steps (0: none).
    pub range: f32,
    /// Scales every displacement (0…1).
    pub strength: f32,
    pub preserve_luma: bool,
    pub weights: [f32; 3],
}

/// The plane position (u, v) of mesh point `index` of a `cols × rows` mesh: hue-sat column `i`,
/// ring `j` at (i / cols, (j + 1) / rows) (the centre, saturation 0, is fixed and not a point);
/// chroma-luma at (i / (cols − 1), j / (rows − 1)). None outside the mesh.
pub fn mesh_point(space: WarpSpace, cols: u32, rows: u32, index: u32) -> Option<[f32; 2]> {
    if cols < 2 || rows < 2 || index >= cols.checked_mul(rows)? {
        return None;
    }
    let (i, j) = (index % cols, index / cols);
    Some(match space {
        WarpSpace::HueSat => [i as f32 / cols as f32, (j + 1) as f32 / rows as f32],
        WarpSpace::ChromaLuma => [i as f32 / (cols - 1) as f32, j as f32 / (rows - 1) as f32],
    })
}

/// Catmull-Rom weights at `t` (0…1) for the samples at −1, 0, 1, 2.
fn catmull_rom(t: f64) -> [f64; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [-0.5 * t3 + t2 - 0.5 * t, 1.5 * t3 - 2.5 * t2 + 1.0, -1.5 * t3 + 2.0 * t2 + 0.5 * t, 0.5 * t3 - 0.5 * t2]
}

/// The falloff weight at `x` = distance / reach (smooth, 1 at 0, 0 from 1 on).
fn falloff(x: f64) -> f64 {
    if x >= 1.0 {
        return 0.0;
    }
    let s = x.max(0.0);
    1.0 - s * s * (3.0 - 2.0 * s)
}

/// Compile a mesh into a warp (module docs). None: nothing moves (no moved point, every move
/// zero, strength 0) or the mesh is out of bounds (sides 2…[`MAX_MESH_SIDE`]); hostile numbers
/// are cleaned (non-finite moves dropped, displacements clamped to ±1, range to
/// 0…[`MAX_RANGE`], strength to 0…1).
pub fn compile(spec: &MeshSpec) -> Option<Warp2D> {
    let (cols, rows) = (spec.cols, spec.rows);
    let side = 2..=MAX_MESH_SIDE;
    if !side.contains(&cols) || !side.contains(&rows) {
        return None;
    }
    let strength = if spec.strength.is_finite() { spec.strength.clamp(0.0, 1.0) } else { 0.0 };
    let range = if spec.range.is_finite() { spec.range.clamp(0.0, MAX_RANGE) } else { 0.0 };
    let n = (cols * rows) as usize;
    let hue_sat = spec.space == WarpSpace::HueSat;
    // per mesh point: displacement and whether it is fixed (moved or pinned)
    let mut disp = vec![[0.0f64; 2]; n];
    let mut moved = vec![false; n];
    let mut fixed = vec![false; n];
    for &(i, d) in spec.moved {
        let (Some(slot), true) = (disp.get_mut(i as usize), d.iter().all(|v| v.is_finite())) else {
            continue;
        };
        let mut d = d.map(|v| f64::from(v.clamp(-1.0, 1.0)));
        // (grey has no hue: the saturation-0 column of chroma-luma only moves in luma)
        if !hue_sat && i % cols == 0 {
            d[0] = 0.0;
        }
        *slot = d;
        if let Some(m) = moved.get_mut(i as usize) {
            *m = true;
        }
        if let Some(f) = fixed.get_mut(i as usize) {
            *f = true;
        }
    }
    for &i in spec.pinned {
        if let Some(f) = fixed.get_mut(i as usize) {
            *f = true;
        }
    }
    if strength == 0.0 || !disp.iter().flatten().any(|v| *v != 0.0) {
        return None;
    }
    // free points follow the moves within `range` mesh steps
    if range > 0.0 {
        let reach = f64::from(range) + 1.0;
        let sources: Vec<(u32, [f64; 2])> = (0..n as u32).filter(|&k| moved.get(k as usize) == Some(&true)).map(|k| (k, disp[k as usize])).collect();
        for k in 0..n as u32 {
            if fixed.get(k as usize) == Some(&true) {
                continue;
            }
            let (ki, kj) = (k % cols, k / cols);
            let (mut sum, mut acc) = (0.0f64, [0.0f64; 2]);
            for &(m, d) in &sources {
                let (mi, mj) = (m % cols, m / cols);
                let mut di = f64::from(ki.abs_diff(mi));
                if hue_sat {
                    di = di.min(f64::from(cols) - di);
                }
                let dj = f64::from(kj.abs_diff(mj));
                let wgt = falloff((di * di + dj * dj).sqrt() / reach);
                sum += wgt;
                acc[0] += wgt * d[0];
                acc[1] += wgt * d[1];
            }
            if sum > 0.0 {
                let norm = sum.max(1.0);
                let mut d = acc.map(|a| a / norm);
                if !hue_sat && ki == 0 {
                    d[0] = 0.0;
                }
                if let Some(slot) = disp.get_mut(k as usize) {
                    *slot = d;
                }
            }
        }
    }
    // the coarse grid: hue-sat gets the fixed centre (saturation 0) as its first row
    let (gc, gr, off) = if hue_sat { (cols, rows + 1, 1u32) } else { (cols, rows, 0u32) };
    let grid = |i: i64, j: i64| -> [f64; 2] {
        let i = if hue_sat { i.rem_euclid(i64::from(gc)) } else { i.clamp(0, i64::from(gc) - 1) };
        let j = j.clamp(0, i64::from(gr) - 1);
        if j < i64::from(off) {
            return [0.0; 2];
        }
        let k = (j - i64::from(off)) * i64::from(cols) + i;
        disp.get(k as usize).copied().unwrap_or([0.0; 2])
    };
    let up = UPSAMPLE;
    let (fc, fr) = if hue_sat { (gc * up, (gr - 1) * up + 1) } else { ((gc - 1) * up + 1, (gr - 1) * up + 1) };
    let s = f64::from(strength);
    let mut field = Vec::with_capacity((fc * fr * 2) as usize);
    for fj in 0..fr {
        let (j0, ty) = (i64::from(fj / up), f64::from(fj % up) / f64::from(up));
        let wy = catmull_rom(ty);
        for fi in 0..fc {
            let (i0, tx) = (i64::from(fi / up), f64::from(fi % up) / f64::from(up));
            let wx = catmull_rom(tx);
            let mut acc = [0.0f64; 2];
            for (b, wyb) in wy.iter().enumerate() {
                for (a, wxa) in wx.iter().enumerate() {
                    let g = grid(i0 + a as i64 - 1, j0 + b as i64 - 1);
                    acc[0] += wxa * wyb * g[0];
                    acc[1] += wxa * wyb * g[1];
                }
            }
            // (clamped to the cell's corners: no overshoot, and exactly zero in a cell whose
            // corners don't move)
            let corners = [grid(i0, j0), grid(i0 + 1, j0), grid(i0, j0 + 1), grid(i0 + 1, j0 + 1)];
            for (k, v) in acc.iter().enumerate() {
                let lo = corners.iter().map(|c| c[k]).fold(f64::INFINITY, f64::min);
                let hi = corners.iter().map(|c| c[k]).fold(f64::NEG_INFINITY, f64::max);
                field.push((v.clamp(lo, hi) * s) as f32);
            }
        }
    }
    Warp2D::new(spec.space, fc, fr, field, spec.preserve_luma, spec.weights)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn spec<'a>(space: WarpSpace, moved: &'a [(u32, [f32; 2])], pinned: &'a [u32], range: f32) -> MeshSpec<'a> {
        MeshSpec { space, cols: 8, rows: 4, moved, pinned, range, strength: 1.0, preserve_luma: false, weights: W }
    }

    fn hsv(c: [f32; 3]) -> [f32; 3] {
        let mx = c[0].max(c[1]).max(c[2]);
        let d = mx - c[0].min(c[1]).min(c[2]);
        [hsv_hue(c, mx, d), d / mx, mx]
    }

    #[test]
    fn nothing_moved_compiles_to_nothing() {
        assert!(compile(&spec(WarpSpace::HueSat, &[], &[1, 2], 2.0)).is_none());
        assert!(compile(&spec(WarpSpace::HueSat, &[(3, [0.0, 0.0])], &[], 2.0)).is_none());
        assert!(compile(&MeshSpec { strength: 0.0, ..spec(WarpSpace::HueSat, &[(3, [0.1, 0.0])], &[], 0.0) }).is_none());
        assert!(compile(&spec(WarpSpace::ChromaLuma, &[(0, [0.3, 0.0])], &[], 0.0)).is_none(), "grey column has no hue to push");
    }

    #[test]
    fn hsv_round_trips() {
        for c in [[0.8f32, 0.2, 0.1], [0.1, 0.9, 0.4], [0.3, 0.2, 0.95], [1.5, 0.4, -0.1], [0.5, 0.5, 0.5]] {
            let [h, s, v] = hsv(c);
            let o = hsv_to_rgb(h, s, v);
            assert!((0..3).all(|k| (o[k] - c[k]).abs() < 1e-5), "{c:?} → {o:?}");
        }
    }

    #[test]
    fn a_moved_hue_point_turns_its_colour_and_leaves_far_colours_bit_exact() {
        // point col 0 (red), ring 2 (sat 0.75) moved 0.1 in hue
        let w = compile(&spec(WarpSpace::HueSat, &[(16, [0.1, 0.0])], &[], 0.0)).unwrap();
        let red = hsv_to_rgb(0.0, 0.75, 0.8);
        let [h, s, v] = hsv(w.apply(red));
        assert!((h - 0.1).abs() < 1e-4 && (s - 0.75).abs() < 1e-4 && (v - 0.8).abs() < 1e-5, "{h} {s} {v}");
        // the opposite hue, and grey, untouched
        for c in [hsv_to_rgb(0.5, 0.75, 0.6), [0.4; 3], [0.0; 3], hsv_to_rgb(0.0, 0.1, 0.5)] {
            assert_eq!(w.apply(c), c);
        }
    }

    #[test]
    fn range_spreads_a_move_and_pins_hold() {
        let near = hsv_to_rgb(0.125, 0.75, 0.8); // column 1, ring 2
        let tight = compile(&spec(WarpSpace::HueSat, &[(16, [0.0, -0.3])], &[], 0.0)).unwrap();
        assert_eq!(tight.apply(near), near, "range 0: neighbours stay put");
        // 1.5 steps away (between the neighbour and the next point): exactly unchanged, no
        // overshoot the other way
        let further = hsv_to_rgb(1.5 / 8.0, 0.75, 0.8);
        assert_eq!(tight.apply(further), further);
        assert_eq!(tight.apply(hsv_to_rgb(1.0 - 1.5 / 8.0, 0.75, 0.8)), hsv_to_rgb(1.0 - 1.5 / 8.0, 0.75, 0.8));
        let wide = compile(&spec(WarpSpace::HueSat, &[(16, [0.0, -0.3])], &[], 2.0)).unwrap();
        assert!(hsv(wide.apply(near))[1] < 0.7, "range 2 pulls the neighbour too");
        let pinned = compile(&spec(WarpSpace::HueSat, &[(16, [0.0, -0.3])], &[17], 2.0)).unwrap();
        let [_, s, _] = hsv(pinned.apply(near));
        assert!((s - 0.75).abs() < 1e-5, "pinned neighbour {s}");
    }

    #[test]
    fn hue_wraps_across_red() {
        // column 7 (hue 0.875) moved +0.2: past red
        let w = compile(&spec(WarpSpace::HueSat, &[(23, [0.2, 0.0])], &[], 0.0)).unwrap();
        let h = hsv(w.apply(hsv_to_rgb(0.875, 0.75, 0.8)))[0];
        assert!((h - 0.075).abs() < 1e-4, "{h}");
        // a lookup just below 1 interpolates towards column 0
        assert!(w.lookup(0.999, 0.75)[0] > 0.0);
    }

    #[test]
    fn chroma_luma_moves_luma_keeping_hue() {
        // column 3 (sat 3/7), row 3 (luma 1): pull luma down by 0.2, mesh 8 × 4
        let w = compile(&spec(WarpSpace::ChromaLuma, &[(3 * 8 + 3, [0.0, -0.2])], &[], 1.0)).unwrap();
        let s = 3.0 / 7.0;
        // a colour with that saturation and luma near 1
        let base = hsv_to_rgb(0.6, s, 1.0);
        let y = W[0] * base[0] + W[1] * base[1] + W[2] * base[2];
        let c = base.map(|q| q / y);
        let o = w.apply(c);
        let y2 = W[0] * o[0] + W[1] * o[1] + W[2] * o[2];
        assert!((y2 - 0.8).abs() < 0.02, "{y2}");
        let (a, b) = (hsv(c), hsv(o));
        assert!((a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4, "{a:?} {b:?}");
    }

    #[test]
    fn preserve_luma_keeps_luma() {
        let m = [(8u32, [0.15f32, 0.1f32])];
        let w = compile(&MeshSpec { preserve_luma: true, ..spec(WarpSpace::HueSat, &m, &[], 1.0) }).unwrap();
        let c = hsv_to_rgb(0.0, 0.5, 0.7);
        let o = w.apply(c);
        let y = |c: [f32; 3]| W[0] * c[0] + W[1] * c[1] + W[2] * c[2];
        assert_ne!(o, c);
        assert!((y(o) - y(c)).abs() < 1e-5);
    }

    #[test]
    fn hostile_meshes_are_refused_or_cleaned() {
        let bad = [(u32::MAX, [0.1, 0.1]), (2, [f32::NAN, 0.1]), (3, [f32::INFINITY, 0.0]), (4, [1e30, -1e30])];
        let w = compile(&MeshSpec { range: f32::NAN, strength: 7.0, ..spec(WarpSpace::HueSat, &bad, &[u32::MAX], 0.0) }).unwrap();
        assert!(w.field().iter().all(|v| v.is_finite() && v.abs() <= 1.2));
        for cols in [0, 1, MAX_MESH_SIDE + 1, u32::MAX] {
            assert!(compile(&MeshSpec { cols, ..spec(WarpSpace::HueSat, &[(0, [0.1, 0.0])], &[], 0.0) }).is_none());
        }
        assert!(compile(&MeshSpec { strength: f32::NAN, ..spec(WarpSpace::HueSat, &[(0, [0.1, 0.0])], &[], 0.0) }).is_none());
        // the largest mesh stays within the field cap
        let big = compile(&MeshSpec { cols: MAX_MESH_SIDE, rows: MAX_MESH_SIDE, ..spec(WarpSpace::HueSat, &[(5, [0.1, 0.1])], &[], 4.0) }).unwrap();
        assert!(big.cols() <= MAX_FIELD_SIDE && big.rows() <= MAX_FIELD_SIDE);
        assert!(Warp2D::new(WarpSpace::HueSat, 2, 2, vec![0.0; 7], false, W).is_none());
        assert!(Warp2D::new(WarpSpace::HueSat, 2, 2, vec![f32::NAN; 8], false, W).is_none());
        // non-finite pixels never panic
        let w = compile(&spec(WarpSpace::ChromaLuma, &[(10, [0.2, 0.2])], &[], 2.0)).unwrap();
        for c in [[f32::NAN; 3], [f32::INFINITY, 0.0, 0.0], [-1.0, -2.0, -3.0], [1e30, 0.0, 1.0]] {
            let _ = w.apply(c);
        }
    }

    #[test]
    fn pinned_and_moved_points_keep_their_own_move_under_a_range() {
        let m = [(16u32, [0.1f32, 0.0f32]), (17, [0.0, -0.2])];
        let w = compile(&spec(WarpSpace::HueSat, &m, &[17], 3.0)).unwrap();
        // point 17 (col 1, ring 2) at hue 1/8, sat 0.75
        let d = w.lookup(1.0 / 8.0, 0.75);
        assert!(d[0].abs() < 1e-6 && (d[1] + 0.2).abs() < 1e-6, "{d:?}");
    }

    #[test]
    fn hue_wraps_at_exactly_one_and_just_below_zero() {
        let w = compile(&spec(WarpSpace::HueSat, &[(23, [0.2, 0.0]), (16, [0.05, 0.1])], &[], 1.0)).unwrap();
        for v in [0.0f32, 0.3, 0.75, 1.0] {
            assert_eq!(w.lookup(1.0, v), w.lookup(0.0, v));
            let (a, b) = (w.lookup(-1e-6, v), w.lookup(1.0 - 1e-6, v));
            assert!((a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5, "{a:?} {b:?}");
        }
    }

    #[test]
    fn near_black_wide_gamut_and_non_finite_pixels_stay_tame() {
        // a dark wide-gamut colour (saturation above 1, luma ~3e-5) on a moved outer ring
        let m = [(24u32, [0.05f32, -0.3f32]), (31, [0.0, 0.0])];
        for preserve in [false, true] {
            let w = compile(&MeshSpec { preserve_luma: preserve, ..spec(WarpSpace::HueSat, &m, &[], 2.0) }).unwrap();
            let c = [0.02, -0.0059, 0.0];
            let o = w.apply(c);
            assert!(o.iter().all(|v| v.is_finite() && v.abs() < 0.1), "{preserve}: {o:?}");
        }
        let w = compile(&spec(WarpSpace::ChromaLuma, &[(26, [0.2, 0.3]), (25, [0.1, -0.3])], &[], 2.0)).unwrap();
        for c in [[0.02f32, -0.0059, 0.0], [0.0011, 0.0, 0.0], [1.8, 0.9, 0.2], [-0.1, 0.4, 0.6]] {
            let o = w.apply(c);
            assert!(o.iter().all(|v| v.is_finite() && v.abs() < 4.0), "{c:?}: {o:?}");
        }
        for c in [[f32::NAN, 0.5, 0.5], [f32::INFINITY, 0.0, 0.0], [0.5, f32::NEG_INFINITY, 0.1]] {
            let o = w.apply(c);
            assert!(o.iter().zip(&c).all(|(a, b)| a.to_bits() == b.to_bits()), "{c:?}: {o:?}");
        }
    }
}
