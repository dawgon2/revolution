//! Following a subject from frame to frame (fork G3 propagation), in the spirit of SAM-based
//! video trackers (SAM-PT, SAM-Track, Track-Anything): the previous mask alone is a weak prompt
//! for an image model, because as soon as the subject touches a neighbour of a similar shape SAM
//! happily returns "the group", and a prior + box grown from that mask locks the mistake in.
//!
//! Each frame [`follow`]:
//! 1. keeps a colour model of the subject (a 16³ RGB histogram) and of the rest of the frame,
//!    learnt on a small copy of the frame (at most [`GRID`] pixels wide);
//! 2. asks the model once with the previous mask's box and positive points sampled deep inside
//!    the previous mask where the frame still has the subject's colours;
//! 3. looks for leaks: parts of the answer in colours of the rest of the frame: new ground
//!    (stricter in the one-pixel band around the previous mask), the previous mask's rim, and
//!    deep inside it unless the subject changed colour as a whole (walking into shadow),
//!    which is learnt instead. When there is a leak it can ask a second time with negative
//!    points in the largest leaks and a box without them;
//! 4. carves any leak still left out of the logits (so it can't seed the next frame either).
//!
//! At most two decoder calls per frame; the image encoder (once per frame) stays the cost.

use crate::{Embedding, LOW_RES, MaskError, Matte, Point, Prompt, Rect, RgbImage, Segment, Segmenter};

/// Width of the analysis copy of the frame (colour model, leaks, points).
pub const GRID: usize = 384;
const BINS: usize = 16 * 16 * 16;

/// Tuning for [`follow`] (the defaults are what the acceptance harness passes with).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowOptions {
    /// Positive points sampled inside the previous mask (0 ..= 8).
    pub positives: usize,
    /// Most negative points per frame (0 ..= 8).
    pub negatives: usize,
    /// Feed the previous logits as the dense prompt.
    pub prior: bool,
    /// Ask for SAM's three candidates on the first call and keep the one that best overlaps the
    /// previous mask (instead of a single answer).
    pub multimask: bool,
    /// Carve leaks left after the second call out of the result.
    pub carve: bool,
    /// Smallest leak region carved (analysis-grid pixels) and how far it is grown first.
    pub carve_min: usize,
    pub carve_grow: usize,
    /// Keep the second answer only when it keeps this share of the first answer's non-leak part.
    pub accept: f32,
    /// Box margin: this share of the previous mask's size per side…
    pub grow: f32,
    /// …and at least this share of the frame.
    pub grow_min: f32,
}

impl Default for FollowOptions {
    fn default() -> Self {
        Self { positives: 3, negatives: 0, prior: false, multimask: false, carve: true, carve_min: 1, carve_grow: 0, accept: 0.95, grow: 0.15, grow_min: 0.02 }
    }
}

/// Most a leak is grown before it is carved (analysis-grid pixels; the cost is linear in it).
pub const MAX_CARVE_GROW: usize = 16;

impl FollowOptions {
    /// Counts capped (points 0 ..= 8, carve growth 0 ..= [`MAX_CARVE_GROW`]); a share that is
    /// not finite takes its default, the rest are clamped to 0 ..= 1.
    pub fn cleaned(&self) -> FollowOptions {
        let d = FollowOptions::default();
        let share = |v: f32, d: f32| if v.is_finite() { v.clamp(0.0, 1.0) } else { d };
        FollowOptions {
            positives: self.positives.min(8),
            negatives: self.negatives.min(8),
            carve_grow: self.carve_grow.min(MAX_CARVE_GROW),
            accept: share(self.accept, d.accept),
            grow: share(self.grow, d.grow),
            grow_min: share(self.grow_min, d.grow_min),
            ..*self
        }
    }
}

/// What [`follow`] carries from frame to frame.
#[derive(Clone, Debug)]
pub struct Track {
    prev: Segment,
    /// Subject / rest-of-frame colour histograms, each summing to 1 (or all 0 when empty).
    fg: Vec<f32>,
    bg: Vec<f32>,
}

/// What happened on one frame (for diagnostics and the UI's status line).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FollowReport {
    pub calls: usize,
    pub positives: Vec<Point>,
    pub negatives: Vec<Point>,
    /// Share of the first answer that was a leak.
    pub leak: f32,
}

impl Track {
    /// Start following `seg` (the user's stroked mask) found on `image`. `None` when it is empty.
    pub fn start(seg: Segment, image: &RgbImage) -> Option<Track> {
        let a = Analysis::new(image, &seg)?;
        let (fg, bg) = a.histograms(&a.mask);
        if fg.iter().all(|v| *v == 0.0) {
            return None;
        }
        Some(Track { prev: seg, fg, bg })
    }
    /// The last mask.
    pub fn segment(&self) -> &Segment {
        &self.prev
    }
}

/// A frame scaled to the analysis grid, with a segment's matte on the same grid.
struct Analysis {
    w: usize,
    h: usize,
    /// Histogram bin per pixel.
    bin: Vec<u16>,
    mask: Vec<bool>,
}

impl Analysis {
    fn new(image: &RgbImage, seg: &Segment) -> Option<Analysis> {
        let (iw, ih) = (image.width(), image.height());
        let w = iw.clamp(1, GRID);
        let h = ((ih as f32 * w as f32 / iw.max(1) as f32).round() as usize).clamp(1, GRID * 4);
        let bin = small_bins(image, w, h);
        let mask = seg.matte(w, h).ok()?.alpha.iter().map(|a| *a >= 128).collect::<Vec<_>>();
        if !mask.iter().any(|m| *m) {
            return None;
        }
        Some(Analysis { w, h, bin, mask })
    }

    fn matte(&self, seg: &Segment) -> Vec<bool> {
        seg.matte(self.w, self.h).map(|m| m.alpha.iter().map(|a| *a >= 128).collect()).unwrap_or_else(|_| vec![false; self.w * self.h])
    }

    /// Subject and rest-of-frame histograms for `mask` (normalised).
    fn histograms(&self, mask: &[bool]) -> (Vec<f32>, Vec<f32>) {
        let (mut fg, mut bg) = (vec![0f32; BINS], vec![0f32; BINS]);
        for (b, m) in self.bin.iter().zip(mask) {
            let h = if *m { &mut fg } else { &mut bg };
            if let Some(v) = h.get_mut(usize::from(*b)) {
                *v += 1.0;
            }
        }
        normalise(&mut fg);
        normalise(&mut bg);
        (fg, bg)
    }
}

fn normalise(h: &mut [f32]) {
    let s: f32 = h.iter().sum();
    if s > 0.0 {
        for v in h.iter_mut() {
            *v /= s;
        }
    }
}

/// Most samples per analysis cell and axis (a 6.2K frame would otherwise read every pixel).
const CELL_SAMPLES: usize = 4;

/// The frame averaged to `w` × `h` (at most [`CELL_SAMPLES`]² evenly spaced samples per cell),
/// as a 16³ colour bin per pixel.
fn small_bins(image: &RgbImage, w: usize, h: usize) -> Vec<u16> {
    let (iw, ih) = (image.width(), image.height());
    let data = image.data();
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let ya = y * ih / h;
        let yb = ((y + 1) * ih / h).clamp(ya + 1, ih.max(ya + 1));
        for x in 0..w {
            let xa = x * iw / w;
            let xb = ((x + 1) * iw / w).clamp(xa + 1, iw.max(xa + 1));
            let mut s = [0u32; 3];
            let mut n = 0u32;
            let (ny, nx) = ((yb - ya).min(CELL_SAMPLES), (xb - xa).min(CELL_SAMPLES));
            for sy in 0..ny {
                let yy = ya + ((2 * sy + 1) * (yb - ya)) / (2 * ny);
                for sx in 0..nx {
                    let xx = xa + ((2 * sx + 1) * (xb - xa)) / (2 * nx);
                    let i = (yy * iw + xx) * 3;
                    if let Some(px) = data.get(i..i + 3) {
                        for (acc, v) in s.iter_mut().zip(px) {
                            *acc += u32::from(*v);
                        }
                        n += 1;
                    }
                }
            }
            let n = n.max(1);
            let [r, g, b] = s.map(|v| ((v / n).min(255) >> 4) as u16);
            out.push((r << 8) | (g << 4) | b);
        }
    }
    out
}

/// Chamfer distance (in pixels, 3-4 metric / 3) from each inside pixel to the nearest outside
/// one; 0 outside. The frame border counts as outside.
fn distance(mask: &[bool], w: usize, h: usize) -> Vec<f32> {
    const BIG: u32 = u32::MAX / 4;
    let mut d: Vec<u32> = mask.iter().map(|m| if *m { BIG } else { 0 }).collect();
    let get = |d: &[u32], x: isize, y: isize| -> u32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize { 0 } else { d.get(y as usize * w + x as usize).copied().unwrap_or(0) }
    };
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            if d.get(i).copied().unwrap_or(0) == 0 {
                continue;
            }
            let v = [get(&d, x - 1, y) + 3, get(&d, x, y - 1) + 3, get(&d, x - 1, y - 1) + 4, get(&d, x + 1, y - 1) + 4];
            if let (Some(slot), Some(m)) = (d.get_mut(i), v.iter().min()) {
                *slot = (*slot).min(*m);
            }
        }
    }
    for y in (0..h as isize).rev() {
        for x in (0..w as isize).rev() {
            let i = y as usize * w + x as usize;
            if d.get(i).copied().unwrap_or(0) == 0 {
                continue;
            }
            let v = [get(&d, x + 1, y) + 3, get(&d, x, y + 1) + 3, get(&d, x + 1, y + 1) + 4, get(&d, x - 1, y + 1) + 4];
            if let (Some(slot), Some(m)) = (d.get_mut(i), v.iter().min()) {
                *slot = (*slot).min(*m);
            }
        }
    }
    d.iter().map(|v| *v as f32 / 3.0).collect()
}

/// Up to `n` points among `ok` pixels: the one deepest in `depth` first, then each next one as
/// far from those chosen as possible (weighted by depth so points stay off the edges).
fn spread_points(ok: &[bool], depth: &[f32], w: usize, n: usize, min_depth: f32) -> Vec<(usize, usize)> {
    let cand: Vec<(usize, f32)> = ok.iter().zip(depth).enumerate().filter(|(_, (o, d))| **o && **d >= min_depth).map(|(i, (_, d))| (i, *d)).collect();
    let mut out: Vec<(usize, usize)> = Vec::new();
    if n == 0 {
        return out;
    }
    let Some(first) = cand.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1)) else { return out };
    out.push((first.0 % w.max(1), first.0 / w.max(1)));
    while out.len() < n {
        let best = cand
            .iter()
            .map(|(i, d)| {
                let (x, y) = (i % w.max(1), i / w.max(1));
                let near = out.iter().map(|(px, py)| (x.abs_diff(*px).pow(2) + y.abs_diff(*py).pow(2)) as f32).fold(f32::MAX, f32::min);
                (x, y, near.sqrt() * d.min(4.0))
            })
            .max_by(|a, b| a.2.total_cmp(&b.2));
        match best {
            Some((x, y, s)) if s > 4.0 * min_depth.max(1.0) => out.push((x, y)),
            _ => break,
        }
    }
    out
}

/// A connected leak region: its pixels and its deepest pixel.
struct Region {
    pixels: Vec<usize>,
    deepest: (usize, usize),
}

/// Connected leak regions (4-connected), largest first.
fn leak_regions(leak: &[bool], w: usize, h: usize) -> Vec<Region> {
    let depth = distance(leak, w, h);
    let mut label = vec![false; leak.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..leak.len() {
        if !leak.get(start).copied().unwrap_or(false) || label.get(start).copied().unwrap_or(true) {
            continue;
        }
        stack.clear();
        stack.push(start);
        if let Some(l) = label.get_mut(start) {
            *l = true;
        }
        let (mut pixels, mut best, mut best_d) = (Vec::new(), start, -1f32);
        while let Some(i) = stack.pop() {
            pixels.push(i);
            let d = depth.get(i).copied().unwrap_or(0.0);
            if d > best_d {
                best_d = d;
                best = i;
            }
            let (x, y) = (i % w, i / w);
            let mut push = |j: usize| {
                if leak.get(j).copied().unwrap_or(false) && !label.get(j).copied().unwrap_or(true) {
                    if let Some(l) = label.get_mut(j) {
                        *l = true;
                    }
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < w {
                push(i + 1);
            }
            if y > 0 {
                push(i - w);
            }
            if y + 1 < h {
                push(i + w);
            }
        }
        out.push(Region { pixels, deepest: (best % w, best / w) });
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.pixels.len()));
    out
}

/// Grow (`r` > 0) a mask by `r` pixels (square neighbourhood; `r` capped at
/// [`MAX_CARVE_GROW`]).
fn dilate(mask: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    let r = r.min(MAX_CARVE_GROW);
    if r == 0 {
        return mask.to_vec();
    }
    let mut rows = vec![false; mask.len()];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w.saturating_sub(1)));
            if let Some(slot) = rows.get_mut(y * w + x) {
                *slot = (a..=b).any(|xx| mask.get(y * w + xx).copied().unwrap_or(false));
            }
        }
    }
    let mut out = vec![false; mask.len()];
    for y in 0..h {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h.saturating_sub(1)));
        for x in 0..w {
            if let Some(slot) = out.get_mut(y * w + x) {
                *slot = (a..=b).any(|yy| rows.get(yy * w + x).copied().unwrap_or(false));
            }
        }
    }
    out
}

fn bounds(mask: &[bool], w: usize, h: usize) -> Option<Rect> {
    let m = Matte { width: w, height: h, alpha: mask.iter().map(|v| if *v { 255 } else { 0 }).collect() };
    m.bounds()
}

fn grown_box(b: Rect, o: &FollowOptions) -> Option<Rect> {
    let gx = ((b.x1 - b.x0) * o.grow).max(o.grow_min);
    let gy = ((b.y1 - b.y0) * o.grow).max(o.grow_min);
    Rect { x0: b.x0 - gx, y0: b.y0 - gy, x1: b.x1 + gx, y1: b.y1 + gy }.sorted()
}

fn overlap(a: &[bool], b: &[bool]) -> f32 {
    let (mut i, mut u) = (0u32, 0u32);
    for (x, y) in a.iter().zip(b) {
        i += u32::from(*x && *y);
        u += u32::from(*x || *y);
    }
    if u == 0 { 0.0 } else { i as f32 / u as f32 }
}

/// Set the logits under `region` (on a `w` × `h` grid covering the frame) to "outside".
fn carve(seg: &mut Segment, region: &[bool], w: usize, h: usize) {
    // (as `Segment::matte` reads it: a cover that is not finite or not positive covers nothing)
    if !(seg.cover.0.is_finite() && seg.cover.1.is_finite()) || w == 0 || h == 0 {
        return;
    }
    let gw = seg.cover.0.clamp(0.0, 1.0) * LOW_RES as f32;
    let gh = seg.cover.1.clamp(0.0, 1.0) * LOW_RES as f32;
    if gw <= 0.0 || gh <= 0.0 {
        return;
    }
    for gy in 0..LOW_RES {
        let y = ((gy as f32 + 0.5) / gh * h as f32) as usize;
        if y >= h {
            break;
        }
        for gx in 0..LOW_RES {
            let x = ((gx as f32 + 0.5) / gw * w as f32) as usize;
            if x >= w {
                break;
            }
            if region.get(y * w + x).copied().unwrap_or(false)
                && let Some(l) = seg.logits.get_mut(gy * LOW_RES + gx)
            {
                *l = l.min(-4.0);
            }
        }
    }
}

/// Follow the subject onto the next frame (`image`, prepared as `embedding`). `Ok(None)` when it
/// is lost (the caller holds the last good mask and asks for a fix stroke).
pub fn follow(
    model: &dyn Segmenter,
    embedding: &Embedding,
    image: &RgbImage,
    track: &Track,
    o: &FollowOptions,
) -> Result<Option<(Track, FollowReport)>, MaskError> {
    let o = &o.cleaned();
    let Some(a) = Analysis::new(image, &track.prev) else { return Ok(None) };
    let (w, h) = (a.w, a.h);
    let prob: Vec<f32> = a
        .bin
        .iter()
        .map(|b| {
            let f = track.fg.get(usize::from(*b)).copied().unwrap_or(0.0);
            let g = track.bg.get(usize::from(*b)).copied().unwrap_or(0.0);
            (f + 1e-5) / (f + g + 2e-5)
        })
        .collect();
    let prev = &a.mask;
    let depth = distance(prev, w, h);
    let ok: Vec<bool> = prob.iter().map(|p| *p >= 0.6).collect();
    let to_point = |(x, y): (usize, usize), positive: bool| Point { x: (x as f32 + 0.5) / w as f32, y: (y as f32 + 0.5) / h as f32, positive };
    let mut positives: Vec<Point> = spread_points(&ok, &depth, w, o.positives, 1.5).into_iter().map(|p| to_point(p, true)).collect();
    if positives.is_empty() && o.positives > 0 {
        // nothing subject-coloured left inside: the deepest point of the previous mask
        positives = spread_points(&vec![true; prev.len()], &depth, w, 1, 0.0).into_iter().map(|p| to_point(p, true)).collect();
    }
    let Some(b) = bounds(prev, w, h) else { return Ok(None) };
    let first = Prompt { points: positives.clone(), rect: grown_box(b, o), prior: o.prior.then(|| track.prev.clone()), prefer_whole: false };
    let mut report = FollowReport { calls: 1, positives: positives.clone(), ..FollowReport::default() };
    let cands = model.candidates(embedding, &first, o.multimask && !positives.is_empty())?;
    // keep the candidate that best overlaps the previous mask (not the model's own score)
    let mut seg = None;
    let mut best = -1.0;
    for c in cands {
        let m = a.matte(&c);
        let s = overlap(&m, prev);
        if s > best {
            best = s;
            seg = Some((c, m));
        }
    }
    let Some((mut seg, mut m)) = seg else { return Ok(None) };

    // leaks: new ground in colours of the rest of the frame (stricter in the one-pixel band
    // around the previous mask, so the mask can't creep into a slightly similar neighbour a
    // pixel a frame), or colours of the rest of the frame on the previous mask's rim, and deep
    // inside it only when the subject's colours did not change as a whole (below).
    let near = dilate(prev, w, h, 1);
    // a colour change of the whole subject (shadow, light, turning round): at least SHIFT of
    // what is deep inside both the previous mask and the answer has colours of the rest of the
    // frame. Then the inside is kept and learnt; otherwise such parts are a neighbour that got
    // in (a bystander the subject passes) and are carved.
    let (mut deep_n, mut deep_off) = (0usize, 0usize);
    for ((pm, (d, p)), mm) in prev.iter().zip(depth.iter().zip(&prob)).zip(&m) {
        if *pm && *mm && *d >= 2.0 {
            deep_n += 1;
            deep_off += usize::from(*p < 0.08);
        }
    }
    let shift = deep_n > 0 && deep_off as f32 >= SHIFT * deep_n as f32;
    let leak_at = |i: usize, p: f32| -> bool {
        if !near.get(i).copied().unwrap_or(false) {
            p < 0.35
        } else if !prev.get(i).copied().unwrap_or(false) {
            p < 0.2
        } else {
            (!shift || depth.get(i).copied().unwrap_or(0.0) < 2.0) && p < 0.08
        }
    };
    let leak_of = |m: &[bool]| -> Vec<bool> { m.iter().zip(&prob).enumerate().map(|(i, (m, p))| *m && leak_at(i, *p)).collect() };
    let leak = leak_of(&m);
    let area = m.iter().filter(|v| **v).count().max(1);
    let leaked = leak.iter().filter(|v| **v).count();
    report.leak = leaked as f32 / area as f32;
    let min_region = (w * h / 4000).max(4);
    if o.negatives > 0 && leaked >= min_region {
        let regions = leak_regions(&leak, w, h);
        let negatives: Vec<Point> = regions.iter().filter(|r| r.pixels.len() >= min_region).take(o.negatives).map(|r| to_point(r.deepest, false)).collect();
        if !negatives.is_empty() {
            let kept: Vec<bool> = m.iter().zip(&leak).map(|(m, l)| *m && !*l).collect();
            let rect = bounds(&kept, w, h).and_then(|b| grown_box(b, &FollowOptions { grow: 0.05, grow_min: 0.005, ..*o }));
            let mut points = positives.clone();
            points.extend(negatives.iter().copied());
            let second = Prompt { points, rect, prior: o.prior.then(|| track.prev.clone()), prefer_whole: false };
            report.calls = 2;
            report.negatives = negatives;
            if let Some(s2) = model.candidates(embedding, &second, false)?.into_iter().next() {
                let m2 = a.matte(&s2);
                // take it unless it dropped most of what wasn't a leak
                let keep = kept.iter().zip(&m2).filter(|(k, n)| **k && **n).count();
                let had = kept.iter().filter(|k| **k).count().max(1);
                if keep as f32 / had as f32 >= o.accept {
                    seg = s2;
                    m = m2;
                }
            }
        }
    }
    if o.carve {
        // regions of at least `carve_min` grid pixels (default 1: every leak pixel)
        let mut rest = vec![false; w * h];
        for r in leak_regions(&leak_of(&m), w, h) {
            if r.pixels.len() >= o.carve_min {
                for i in r.pixels {
                    if let Some(v) = rest.get_mut(i) {
                        *v = true;
                    }
                }
            }
        }
        if rest.iter().any(|v| *v) {
            carve(&mut seg, &dilate(&rest, w, h, o.carve_grow), w, h);
            m = a.matte(&seg);
        }
    }
    if !m.iter().any(|v| *v) {
        return Ok(None);
    }
    // learn: the subject slowly from pixels that already looked like it, and after a colour
    // change of the whole subject also from what is deep inside both masks whatever its colour;
    // the rest of the frame faster but not at once (a colour absorbed by the mask keeps some
    // weight as "not the subject")
    let (fg_new, bg_new) = a.histograms(&m);
    let sure: Vec<bool> = m.iter().zip(&prob).map(|(m, p)| *m && *p >= 0.5).collect();
    let deep: Vec<bool> = m.iter().zip(&depth).map(|(m, d)| shift && *m && *d >= 2.0).collect();
    let (fg_sure, _) = a.histograms(&sure);
    let (fg_deep, _) = a.histograms(&deep);
    let src = if sure.iter().any(|v| *v) { &fg_sure } else { &fg_new };
    let fg = blend(&blend(&track.fg, src, FG_RATE), &fg_deep, FG_DEEP_RATE);
    let bg = blend(&track.bg, &bg_new, BG_RATE);
    Ok(Some((Track { prev: seg, fg, bg }, report)))
}

/// How fast the colour models learn a frame (share of the new histogram).
const FG_RATE: f32 = 0.1;
/// …from the inside after a colour change of the whole subject ([`SHIFT`]).
const FG_DEEP_RATE: f32 = 0.3;
/// Share of the inside in colours of the rest of the frame that counts as a colour change.
const SHIFT: f32 = 0.1;
const BG_RATE: f32 = 0.3;

/// `old` moved `rate` of the way to `new` (both normalised); `old` when `new` is empty.
fn blend(old: &[f32], new: &[f32], rate: f32) -> Vec<f32> {
    if !new.iter().any(|v| *v > 0.0) {
        return old.to_vec();
    }
    if !old.iter().any(|v| *v > 0.0) {
        return new.to_vec();
    }
    old.iter().zip(new).map(|(o, n)| o * (1.0 - rate) + n * rate).collect()
}
