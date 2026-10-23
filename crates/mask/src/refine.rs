//! Refining a matte before it limits a node (fork G3): grow / shrink, clean black / white,
//! soften, invert. Pure CPU, O(1) per pixel whatever the radius (sliding min / max with a
//! monotonic queue, running-sum box blurs), so a 6.2K matte with a large radius stays cheap.

use std::collections::VecDeque;

use crate::{MaskError, Matte};

/// Largest grow / shrink / soften radius in pixels after scaling to the frame.
pub const MAX_RADIUS: usize = 1024;

/// Refine settings in frame-independent units (pixels of a 1080-line frame, like Resolve's
/// sliders scale with the picture).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Refine {
    /// > 0 grows the subject, < 0 shrinks it.
    pub grow: f32,
    /// Edge blur radius.
    pub soften: f32,
    /// 0..1: values below become 0.
    pub clean_black: f32,
    /// 0..1: values above 1 − this become 1.
    pub clean_white: f32,
    pub invert: bool,
}

impl Refine {
    /// Nothing to do.
    pub fn is_identity(&self) -> bool {
        // Any non-zero radius counts: 0.4 rounds to 0 at 1080 lines but to 2 px at 4160.
        let r = |v: f32| !v.is_finite() || v == 0.0;
        let off = |v: f32| v.is_nan() || v <= 0.0;
        r(self.grow) && r(self.soften) && off(self.clean_black) && off(self.clean_white) && !self.invert
    }
}

/// A 1080-line-frame distance scaled to a frame `height` lines tall, in whole pixels, capped.
fn radius(v: f32, height: usize) -> usize {
    if !v.is_finite() {
        return 0;
    }
    let px = (v.abs() * height as f32 / 1080.0).round();
    if px >= MAX_RADIUS as f32 { MAX_RADIUS } else { px.max(0.0) as usize }
}

/// The refined matte. A matte whose data doesn't match its size is refused.
pub fn refine(m: &Matte, r: &Refine) -> Result<Matte, MaskError> {
    let (w, h) = (m.width, m.height);
    if w == 0 || h == 0 || w.checked_mul(h) != Some(m.alpha.len()) {
        return Err(MaskError::BadImage(format!("a {w}×{h} matte with {} values", m.alpha.len())));
    }
    let mut a: Vec<f32> = Vec::new();
    a.try_reserve_exact(m.alpha.len()).map_err(|_| MaskError::BadImage(format!("no memory for a {w}×{h} matte")))?;
    a.extend(m.alpha.iter().map(|v| f32::from(*v) / 255.0));

    let g = radius(r.grow, h);
    if g > 0 {
        let grow = r.grow > 0.0;
        separable(&mut a, w, h, |line| slide_extreme(line, g, grow));
    }
    let cb = if r.clean_black.is_finite() { r.clean_black.clamp(0.0, 1.0) } else { 0.0 };
    let cw = if r.clean_white.is_finite() { r.clean_white.clamp(0.0, 1.0) } else { 0.0 };
    if cb > 0.0 || cw > 0.0 {
        let hi = (1.0 - cw).max(cb + 1e-3);
        for v in &mut a {
            *v = ((*v - cb) / (hi - cb)).clamp(0.0, 1.0);
        }
    }
    let s = radius(r.soften, h);
    if s > 0 {
        // Three box passes ≈ a Gaussian; each box has radius s / 2 (at least 1).
        let b = (s / 2).max(1);
        for _ in 0..3 {
            separable(&mut a, w, h, |line| box_blur(line, b));
        }
    }
    let alpha = a.iter().map(|v| {
        let v = if r.invert { 1.0 - v } else { *v };
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    });
    Ok(Matte { width: w, height: h, alpha: alpha.collect() })
}

/// Run `f` on every row, then every column, of a `w` × `h` image.
fn separable(a: &mut [f32], w: usize, h: usize, f: impl Fn(&[f32]) -> Vec<f32>) {
    for row in a.chunks_mut(w) {
        let out = f(row);
        for (slot, v) in row.iter_mut().zip(out) {
            *slot = v;
        }
    }
    let mut col = vec![0f32; h];
    for x in 0..w {
        for (y, c) in col.iter_mut().enumerate() {
            *c = a.get(y * w + x).copied().unwrap_or(0.0);
        }
        let out = f(&col);
        for (y, v) in out.iter().enumerate() {
            if let Some(slot) = a.get_mut(y * w + x) {
                *slot = *v;
            }
        }
    }
}

/// Max (or min) over the window [i − r, i + r] (clipped at the ends), O(n) with a monotonic queue.
fn slide_extreme(line: &[f32], r: usize, max: bool) -> Vec<f32> {
    let n = line.len();
    let better = |a: f32, b: f32| if max { a >= b } else { a <= b };
    let mut q: VecDeque<usize> = VecDeque::new();
    let mut out = Vec::with_capacity(n);
    let mut next = 0;
    for i in 0..n {
        let hi = (i + r).min(n - 1);
        while next <= hi {
            let v = line.get(next).copied().unwrap_or(0.0);
            while q.back().is_some_and(|&j| better(v, line.get(j).copied().unwrap_or(0.0))) {
                q.pop_back();
            }
            q.push_back(next);
            next += 1;
        }
        while q.front().is_some_and(|&j| j + r < i) {
            q.pop_front();
        }
        out.push(q.front().and_then(|&j| line.get(j)).copied().unwrap_or(0.0));
    }
    out
}

/// Mean over [i − r, i + r], edges clamped (the edge value repeats), O(n) with a running sum.
fn box_blur(line: &[f32], r: usize) -> Vec<f32> {
    let n = line.len();
    if n == 0 {
        return Vec::new();
    }
    let at = |i: isize| line.get(i.clamp(0, n as isize - 1) as usize).copied().unwrap_or(0.0);
    let r = r.min(n) as isize;
    let k = (2 * r + 1) as f32;
    let mut sum: f32 = (-r..=r).map(at).sum();
    let mut out = Vec::with_capacity(n);
    for i in 0..n as isize {
        out.push(sum / k);
        sum += at(i + r + 1) - at(i - r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(w: usize, h: usize, x0: usize, x1: usize, y0: usize, y1: usize) -> Matte {
        let alpha = (0..w * h).map(|i| if (x0..x1).contains(&(i % w)) && (y0..y1).contains(&(i / w)) { 255 } else { 0 }).collect();
        Matte { width: w, height: h, alpha }
    }

    fn count(m: &Matte) -> usize {
        m.alpha.iter().filter(|a| **a >= 128).count()
    }

    #[test]
    fn identity_changes_nothing() {
        let m = square(40, 30, 10, 20, 5, 15);
        assert!(Refine::default().is_identity());
        assert!(!Refine { soften: 0.4, ..Refine::default() }.is_identity());
        assert_eq!(refine(&m, &Refine::default()).unwrap(), m);
    }

    #[test]
    fn grow_and_shrink_move_the_edge_by_the_scaled_radius() {
        // A 1080-line frame: 2 px of grow is 2 px.
        let m = square(100, 1080, 40, 60, 500, 520);
        let grown = refine(&m, &Refine { grow: 2.0, ..Refine::default() }).unwrap();
        assert_eq!(count(&grown), 24 * 24);
        let shrunk = refine(&m, &Refine { grow: -2.0, ..Refine::default() }).unwrap();
        assert_eq!(count(&shrunk), 16 * 16);
        // A 540-line frame: 4 "1080 px" is 2 px.
        let half = square(100, 540, 40, 60, 200, 220);
        assert_eq!(count(&refine(&half, &Refine { grow: 4.0, ..Refine::default() }).unwrap()), 24 * 24);
        // Shrinking past the subject empties it; huge values are capped, not allocated.
        assert_eq!(count(&refine(&m, &Refine { grow: -50.0, ..Refine::default() }).unwrap()), 0);
        assert_eq!(count(&refine(&m, &Refine { grow: f32::MAX, ..Refine::default() }).unwrap()), 100 * 1080);
    }

    #[test]
    fn clean_soften_and_invert() {
        let mut m = square(64, 64, 16, 48, 16, 48);
        m.alpha[0] = 40; // a speck of noise outside
        m.alpha[64 * 32 + 32] = 200; // a soft spot inside
        let clean = refine(&m, &Refine { clean_black: 0.2, clean_white: 0.25, ..Refine::default() }).unwrap();
        assert_eq!(clean.alpha[0], 0);
        assert_eq!(clean.alpha[64 * 32 + 32], 255);
        let soft = refine(&square(64, 64, 16, 48, 16, 48), &Refine { soften: 40.0, ..Refine::default() }).unwrap();
        let edge = soft.alpha[64 * 32 + 16];
        assert!(edge > 60 && edge < 200, "edge {edge}");
        assert_eq!(soft.alpha[64 * 32 + 32], 255);
        assert_eq!(soft.alpha[0], 0);
        let inv = refine(&square(4, 4, 0, 2, 0, 4), &Refine { invert: true, ..Refine::default() }).unwrap();
        assert_eq!(inv.alpha, vec![0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn hostile_mattes_and_values_never_panic() {
        assert!(refine(&Matte { width: 3, height: 3, alpha: vec![0; 8] }, &Refine::default()).is_err());
        assert!(refine(&Matte { width: 0, height: 0, alpha: vec![] }, &Refine::default()).is_err());
        assert!(refine(&Matte { width: usize::MAX, height: 2, alpha: vec![] }, &Refine::default()).is_err());
        let one = Matte { width: 1, height: 1, alpha: vec![255] };
        let nan = Refine { grow: f32::NAN, soften: f32::INFINITY, clean_black: f32::NAN, clean_white: 1.0, invert: false };
        assert!(refine(&one, &nan).is_ok());
        let both = Refine { clean_black: 1.0, clean_white: 1.0, ..Refine::default() };
        assert!(refine(&square(8, 8, 2, 6, 2, 6), &both).is_ok());
        assert!(refine(&one, &Refine { soften: 1e6, grow: -1e6, ..Refine::default() }).is_ok());
    }
}
