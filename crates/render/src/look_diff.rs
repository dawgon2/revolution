//! Measured "theirs vs yours" look difference (fork AI-PLAN REF.2): given the measured facts of
//! a reference frame ("theirs") and of the user's own frame ("yours"), say in plain words what
//! differs: exposure, contrast, black level (faded or crushed), shadow and highlight casts
//! (teal / orange split toning), saturation, skin, letterbox or pillarbox bars and grain.
//!
//! [`LookFacts`] adds to [`PictureFacts`] the things a look is made of that the scopes-style
//! fact sheet does not carry. Everything is measured, nothing is guessed: frames are working
//! linear light (premultiplied RGBA, 1.0 = reference white), non-finite or transparent pixels
//! are skipped, at most about [`MAX_SAMPLES`] pixels are read, and a frame that cannot be
//! measured gives `None`. Letterbox / pillarbox bars are excluded from every other statistic.

use filmcraft_color::spaces::{apply3, gamut_matrix, to_f32};
use filmcraft_color::{Gamut, WorkingSpace};

use crate::Image;
use crate::color_match::oklab;
use crate::grade_match::JND_CAST;
use crate::picture_facts::{MAX_SAMPLES, MIDDLE_GREY, NEUTRAL_MIN_SHARE, PictureFacts, SKIN_MIN_SHARE, luma, quantile, working_gamut};

/// A pixel whose largest channel is at or below this is black bar.
pub const BAR_LEVEL: f32 = 0.01;
/// A row (column) is a bar when at least this share of its sampled pixels is black.
pub const BAR_ROW_SHARE: f32 = 0.98;
/// Bars (both sides together) under this share of the frame size are not reported.
pub const BAR_MIN_SHARE: f32 = 0.04;
/// Offset in the grain ratio so flat black does not blow it up.
const GRAIN_OFFSET: f32 = 0.02;

/// Mids must differ by this many stops for Brighter / Darker.
pub const EXPOSURE_STOPS: f32 = 0.33;
/// The 5-95 % contrast must differ by this many stops for MoreContrast / LessContrast.
pub const CONTRAST_STOPS: f32 = 0.5;
/// Theirs' black level must sit this many stops away from yours for FadedBlacks / CrushedBlacks.
pub const BLACK_STOPS: f32 = 1.5;
/// Blacks above this (stops against middle grey) count as visibly lifted at all.
pub const LIFTED_BLACK_STOPS: f32 = -4.5;
/// Crushed blacks also count when the share of crushed pixels differs by at least this.
pub const CRUSHED_SHARE: f32 = 0.05;
/// A cast difference counts from this many just-noticeable casts ([`JND_CAST`]) of Oklab distance.
pub const CAST_JND_MULTIPLE: f32 = 2.0;
/// Overall warmth (the near-neutral cast) differs by this many JND casts for Warmer / Cooler.
pub const WARMTH_JND_MULTIPLE: f32 = 3.0;
/// Mean chroma ratio (theirs / yours) at or above this is MoreSaturated.
pub const SATURATION_UP: f32 = 1.25;
/// Mean chroma ratio at or below this is LessSaturated.
pub const SATURATION_DOWN: f32 = 0.8;
/// Skin hue offsets must differ by this many degrees.
pub const SKIN_DEG: f32 = 5.0;
/// Grain ratio (one over the other) needed.
pub const GRAIN_RATIO: f32 = 2.0;
/// ...and the grain estimates must differ by at least this much.
pub const GRAIN_FLOOR: f32 = 0.02;

const MAX_LINES: usize = 8;
const MAX_LINE_CHARS: usize = 160;

/// One frame's measured look facts.
#[derive(Clone, Debug, PartialEq)]
pub struct LookFacts {
    /// The ordinary picture facts (measured inside any bars).
    pub facts: PictureFacts,
    /// 1st-percentile luminance against middle grey in stops: lifted / faded blacks sit much
    /// higher than a normal picture's.
    pub black_stops: f32,
    /// Mean Oklab (a, b) of the darkest third of the pixels by luminance.
    pub shadow_cast: [f32; 2],
    /// Mean Oklab (a, b) of the brightest third of the pixels by luminance.
    pub highlight_cast: [f32; 2],
    /// Share of the height covered by matching black bars at the top and bottom.
    pub letterbox: Option<f32>,
    /// Share of the width covered by matching black bars left and right.
    pub pillarbox: Option<f32>,
    /// Noise estimate: median |luminance - mean of the 4 neighbours| / (local mean + 0.02).
    pub grain: f32,
}

/// Linear Rec. 709 colour of one RGBA pixel, `None` when transparent or non-finite.
fn pixel709(p: &[f32], m: &Option<[[f32; 3]; 3]>) -> Option<[f32; 3]> {
    let &[r, g, b, a] = p.first_chunk::<4>()?;
    if ![r, g, b, a].iter().all(|v| v.is_finite()) || a <= 1e-3 {
        return None;
    }
    let a = a.min(1.0);
    let mut c = [r / a, g / a, b / a].map(|v| v.clamp(-1e6, 1e6));
    if let Some(m) = m {
        c = apply3(m, c);
    }
    c.iter().all(|v| v.is_finite()).then_some(c)
}

/// Whether the sampled pixels of one line (`len` pixels, every `pixel_step`-th, starting at float
/// offset `start` and `stride_floats` apart) are black bar.
fn line_is_bar(px: &[f32], start: usize, len: usize, pixel_step: usize, stride_floats: usize) -> bool {
    let (mut total, mut black) = (0usize, 0usize);
    for i in (0..len).step_by(pixel_step.max(1)) {
        let Some(at) = i.checked_mul(stride_floats).and_then(|o| o.checked_add(start)) else { return false };
        let Some(c) = at.checked_add(3).and_then(|end| px.get(at..end)) else { return false };
        total += 1;
        if c.iter().all(|v| v.is_finite() && *v <= BAR_LEVEL) {
            black += 1;
        }
    }
    total > 0 && (black as f32) >= BAR_ROW_SHARE * (total as f32)
}

/// Bars along one axis of `lines` lines. Returns (leading, trailing) counts.
fn bar_counts(lines: usize, is_bar: impl Fn(usize) -> bool) -> (usize, usize) {
    let lead = (0..lines).take_while(|&i| is_bar(i)).count();
    if lead >= lines {
        return (lead, 0);
    }
    let trail = (0..lines).rev().take_while(|&i| is_bar(i)).count();
    (lead, trail)
}

/// Matching bars on both sides, big enough to report: the share of the axis they cover.
fn matching_bars(lead: usize, trail: usize, size: usize) -> Option<f32> {
    let sum = lead.checked_add(trail)?;
    let matched = lead > 0 && trail > 0 && lead.abs_diff(trail) <= sum / 4 + 1 && sum < size;
    let share = sum as f32 / size.max(1) as f32;
    (matched && share >= BAR_MIN_SHARE).then_some(share)
}

/// The picture next to a bar must be at least this bright on average (max channel, linear) and
/// [`BAR_EDGE_RATIO`] times brighter than the bar's last line: a bar ends at a visible edge. A
/// night sky that fades out to black is not a letterbox.
pub const BAR_EDGE_LEVEL: f32 = 0.012;
pub const BAR_EDGE_RATIO: f32 = 4.0;

/// Whether the line `inside` (first picture line) is a visible edge after the bar line `bar`.
fn is_edge(inside: f32, bar: f32) -> bool {
    inside >= BAR_EDGE_LEVEL && inside >= BAR_EDGE_RATIO * bar
}

/// Mean of the max channel along a line (`len` pixels from `start`, `stride_floats` apart),
/// finite pixels only; 0 when there are none.
fn line_level(px: &[f32], start: usize, len: usize, pixel_step: usize, stride_floats: usize) -> f32 {
    let (mut sum, mut n) = (0f64, 0u32);
    for i in (0..len).step_by(pixel_step.max(1)) {
        let Some(at) = i.checked_mul(stride_floats).and_then(|o| o.checked_add(start)) else { break };
        let Some(c) = at.checked_add(3).and_then(|end| px.get(at..end)) else { break };
        let m = c.iter().copied().filter(|v| v.is_finite()).fold(0f32, f32::max);
        sum += f64::from(m.min(1e6));
        n = n.saturating_add(1);
    }
    if n == 0 { 0.0 } else { (sum / f64::from(n)) as f32 }
}

/// The picture rectangle `(x0, x1, y0, y1)` inside bars, plus the two bar shares.
fn find_bars(img: &Image, px: &[f32]) -> ((usize, usize, usize, usize), Option<f32>, Option<f32>) {
    let (w, h) = (img.w, img.h);
    let sx = w.div_ceil(512).max(1);
    let (top, bottom) = bar_counts(h, |y| line_is_bar(px, y.saturating_mul(w).saturating_mul(4), w, sx, 4));
    let row = |y: usize| line_level(px, y.saturating_mul(w).saturating_mul(4), w, sx, 4);
    let last = h.saturating_sub(bottom);
    let letterbox = matching_bars(top, bottom, h)
        .filter(|_| is_edge(row(top), row(top.saturating_sub(1))) && is_edge(row(last.saturating_sub(1)), row(last.min(h.saturating_sub(1)))));
    let (y0, y1) = if letterbox.is_some() { (top, h.saturating_sub(bottom)) } else { (0, h) };
    let rows = y1.saturating_sub(y0);
    let sy = rows.div_ceil(512).max(1);
    let (left, right) = bar_counts(w, |x| line_is_bar(px, y0.saturating_mul(w).saturating_add(x).saturating_mul(4), rows, sy, w.saturating_mul(4)));
    let col = |x: usize| line_level(px, y0.saturating_mul(w).saturating_add(x).saturating_mul(4), rows, sy, w.saturating_mul(4));
    let lastx = w.saturating_sub(right);
    let pillarbox = matching_bars(left, right, w)
        .filter(|_| is_edge(col(left), col(left.saturating_sub(1))) && is_edge(col(lastx.saturating_sub(1)), col(lastx.min(w.saturating_sub(1)))));
    let (x0, x1) = if pillarbox.is_some() { (left, w.saturating_sub(right)) } else { (0, w) };
    ((x0, x1, y0, y1), letterbox, pillarbox)
}

/// Copy of the sub-rectangle (only called when bars were found).
fn crop(img: &Image, px: &[f32], (x0, x1, y0, y1): (usize, usize, usize, usize)) -> Option<Image> {
    let (cw, ch) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
    if cw == 0 || ch == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(cw.checked_mul(ch)?.checked_mul(4)?);
    for y in y0..y1 {
        let a = y.checked_mul(img.w)?.checked_add(x0)?.checked_mul(4)?;
        out.extend_from_slice(px.get(a..a.checked_add(cw.checked_mul(4)?)?)?);
    }
    Some(Image { w: cw, h: ch, px: out })
}

fn mean_ab(samples: &[(f32, f32, f32)]) -> [f32; 2] {
    if samples.is_empty() {
        return [0.0; 2];
    }
    let n = samples.len() as f64;
    let (a, b) = samples.iter().fold((0f64, 0f64), |s, v| (s.0 + f64::from(v.1), s.1 + f64::from(v.2)));
    let out = [(a / n) as f32, (b / n) as f32];
    if out.iter().all(|v| v.is_finite()) { out } else { [0.0; 2] }
}

impl LookFacts {
    /// Measure a frame in `working`'s linear light. `None` where [`PictureFacts::measure`] is.
    pub fn measure(img: &Image, working: WorkingSpace) -> Option<LookFacts> {
        let total = img.w.checked_mul(img.h)?;
        let px = img.px.get(..total.checked_mul(4)?)?;
        PictureFacts::measure(img, working)?;
        let (rect, letterbox, pillarbox) = find_bars(img, px);
        let cropped = if letterbox.is_some() || pillarbox.is_some() { crop(img, px, rect) } else { None };
        let inner = cropped.as_ref().unwrap_or(img);
        let facts = PictureFacts::measure(inner, working)?;
        let (w, h) = (inner.w, inner.h);
        let ipx = inner.px.get(..w.checked_mul(h)?.checked_mul(4)?)?;

        let g = working_gamut(working);
        let m = (g != Gamut::Bt709).then(|| to_f32(&gamut_matrix(g, Gamut::Bt709)));
        let at = |x: usize, y: usize| -> Option<[f32; 3]> {
            let o = y.checked_mul(w)?.checked_add(x)?.checked_mul(4)?;
            pixel709(ipx.get(o..o.checked_add(4)?)?, &m)
        };
        let step = ((w as f64 * h as f64) / MAX_SAMPLES as f64).sqrt().ceil().max(1.0) as usize;
        let mut samples: Vec<(f32, f32, f32)> = Vec::new();
        let mut grain = Vec::new();
        for y in (0..h).step_by(step) {
            for x in (0..w).step_by(step) {
                let Some(c) = at(x, y) else { continue };
                let l = luma(c).max(0.0);
                let lab = oklab(c);
                if !(l.is_finite() && lab[1].is_finite() && lab[2].is_finite()) {
                    continue;
                }
                samples.push((l, lab[1], lab[2]));
                if x >= 1 && y >= 1 && x + 1 < w && y + 1 < h {
                    let nb = [at(x - 1, y), at(x + 1, y), at(x, y - 1), at(x, y + 1)];
                    if nb.iter().all(Option::is_some) {
                        let mean = nb.iter().flatten().map(|c| luma(*c).max(0.0)).sum::<f32>() / 4.0;
                        let d = (l - mean).abs() / (mean + GRAIN_OFFSET);
                        if d.is_finite() {
                            grain.push(d);
                        }
                    }
                }
            }
        }
        if samples.is_empty() {
            return None;
        }
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        let lumas: Vec<f32> = samples.iter().map(|s| s.0).collect();
        let black = quantile(&lumas, 0.01)?.max(1e-5);
        let third = (samples.len() / 3).max(1);
        let shadow_cast = mean_ab(samples.get(..third)?);
        let highlight_cast = mean_ab(samples.get(samples.len().saturating_sub(third)..)?);
        grain.sort_by(f32::total_cmp);
        let grain = quantile(&grain, 0.5).unwrap_or(0.0);
        let out = LookFacts { facts, black_stops: (black / MIDDLE_GREY).log2(), shadow_cast, highlight_cast, letterbox, pillarbox, grain };
        [out.black_stops, out.grain].iter().all(|v| v.is_finite()).then_some(out)
    }
}

/// One named difference between a reference look and yours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookTag {
    Brighter,
    Darker,
    MoreContrast,
    LessContrast,
    FadedBlacks,
    CrushedBlacks,
    WarmerOverall,
    CoolerOverall,
    TealShadows,
    /// Warm (orange) highlights.
    OrangeHighlights,
    WarmShadows,
    CoolHighlights,
    /// Teal shadows and warm highlights together.
    SplitToneTealOrange,
    MoreSaturated,
    LessSaturated,
    SkinWarmer,
    SkinCooler,
    Letterbox,
    Pillarbox,
    MoreGrain,
    LessGrain,
}

/// The measured difference: tags for programs, short plain lines for people and the assistant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookDiff {
    pub tags: Vec<LookTag>,
    pub lines: Vec<String>,
}

impl LookDiff {
    /// The lines under a heading, ready to hand to the assistant.
    pub fn sheet(&self) -> String {
        let mut s = String::from("Theirs vs yours (measured):");
        for l in &self.lines {
            s.push('\n');
            s.push_str(l);
        }
        s
    }
}

fn f1(v: f32) -> String {
    format!("{:.1}", if v.is_finite() { v } else { 0.0 })
}

fn pct(v: f32) -> String {
    format!("{:.0}", if v.is_finite() { v * 100.0 } else { 0.0 })
}

/// Short plain description of a cast vector, as a verb phrase.
fn cast_phrase(c: [f32; 2]) -> String {
    let [a, b] = c;
    if (a * a + b * b).sqrt() < CAST_JND_MULTIPLE * JND_CAST {
        return "are neutral".to_string();
    }
    let word = if a < 0.0 && b < 0.0 {
        "teal"
    } else if b.abs() >= a.abs() {
        if b > 0.0 { "warm" } else { "cool" }
    } else if a > 0.0 {
        "magenta"
    } else {
        "green"
    };
    format!("lean {word}")
}

fn clip_line(s: String) -> String {
    if s.chars().count() > MAX_LINE_CHARS { s.chars().take(MAX_LINE_CHARS - 3).collect::<String>() + "..." } else { s }
}

/// What differs between the reference look (`theirs`) and `yours`.
pub fn difference(theirs: &LookFacts, yours: &LookFacts) -> LookDiff {
    use LookTag::*;
    let (t, y) = (&theirs.facts, &yours.facts);
    let mut tags = Vec::new();
    let mut lines = Vec::new();

    let ex = t.mids_stops - y.mids_stops;
    if ex.abs() >= EXPOSURE_STOPS {
        tags.push(if ex > 0.0 { Brighter } else { Darker });
        lines.push(format!("Exposure: theirs is about {} stop {} in the mids.", f1(ex.abs()), if ex > 0.0 { "brighter" } else { "darker" }));
    }

    let ct = t.contrast_stops - y.contrast_stops;
    if ct.abs() >= CONTRAST_STOPS {
        tags.push(if ct > 0.0 { MoreContrast } else { LessContrast });
        lines.push(format!(
            "Contrast: theirs spans {} stops (5-95%) against your {}; theirs is {}.",
            f1(t.contrast_stops),
            f1(y.contrast_stops),
            if ct > 0.0 { "punchier" } else { "flatter" }
        ));
    }

    let bd = theirs.black_stops - yours.black_stops;
    if bd >= BLACK_STOPS && theirs.black_stops > LIFTED_BLACK_STOPS {
        tags.push(FadedBlacks);
        lines.push(format!(
            "Blacks: theirs are lifted (faded) to about {} stops below middle grey; yours sit at {}.",
            f1(-theirs.black_stops),
            f1(-yours.black_stops)
        ));
    } else if bd <= -BLACK_STOPS && (yours.black_stops > LIFTED_BLACK_STOPS || t.crushed - y.crushed >= CRUSHED_SHARE) {
        tags.push(CrushedBlacks);
        lines.push(format!(
            "Blacks: theirs are deeper (crushed) at about {} stops below middle grey; yours sit at {}.",
            f1(-theirs.black_stops),
            f1(-yours.black_stops)
        ));
    }

    let limit = CAST_JND_MULTIPLE * JND_CAST;
    let ds = [theirs.shadow_cast[0] - yours.shadow_cast[0], theirs.shadow_cast[1] - yours.shadow_cast[1]];
    let dh = [theirs.highlight_cast[0] - yours.highlight_cast[0], theirs.highlight_cast[1] - yours.highlight_cast[1]];
    let teal = ds[0] < 0.0 && ds[1] < 0.0 && (ds[0] * ds[0] + ds[1] * ds[1]).sqrt() >= limit;
    let warm_shadows = !teal && ds[1] >= limit;
    let warm_high = dh[1] >= limit;
    let cool_high = dh[1] <= -limit;
    if teal {
        tags.push(TealShadows);
    }
    if warm_shadows {
        tags.push(WarmShadows);
    }
    if warm_high {
        tags.push(OrangeHighlights);
    }
    if cool_high {
        tags.push(CoolHighlights);
    }
    if teal && warm_high {
        tags.push(SplitToneTealOrange);
    }
    if teal || warm_shadows {
        lines.push(format!("Shadows: theirs {}; yours {}.", cast_phrase(theirs.shadow_cast), cast_phrase(yours.shadow_cast)));
    }
    if warm_high || cool_high {
        lines.push(format!("Highlights: theirs {}; yours {}.", cast_phrase(theirs.highlight_cast), cast_phrase(yours.highlight_cast)));
    }

    // Colour: overall warmth, saturation, skin
    let mut colour = Vec::new();
    if t.neutral_share >= NEUTRAL_MIN_SHARE && y.neutral_share >= NEUTRAL_MIN_SHARE {
        let w = t.cast[1] - y.cast[1];
        if w.abs() >= WARMTH_JND_MULTIPLE * JND_CAST {
            tags.push(if w > 0.0 { WarmerOverall } else { CoolerOverall });
            colour.push(format!("theirs is {} overall", if w > 0.0 { "warmer" } else { "cooler" }));
        }
    }
    if y.chroma > 1e-4 {
        let r = t.chroma / y.chroma;
        if r >= SATURATION_UP {
            tags.push(MoreSaturated);
            colour.push(format!("theirs is more saturated (about {}x)", f1(r)));
        } else if r <= SATURATION_DOWN {
            tags.push(LessSaturated);
            colour.push(format!("theirs is less saturated (about {}x)", f1(r)));
        }
    }
    if let (Some(a), Some(b)) = (t.skin_offset_deg, y.skin_offset_deg) {
        let d = a - b;
        if t.skin_share >= SKIN_MIN_SHARE && y.skin_share >= SKIN_MIN_SHARE && d.is_finite() && d.abs() >= SKIN_DEG {
            tags.push(if d > 0.0 { SkinWarmer } else { SkinCooler });
            colour.push(format!("theirs skin is about {:.0} degrees {}", d.abs(), if d > 0.0 { "more yellow" } else { "more red" }));
        }
    }
    if !colour.is_empty() {
        lines.push(format!("Colour: {}.", colour.join("; ")));
    }

    let mut frame = Vec::new();
    if let (Some(a), None) = (theirs.letterbox, yours.letterbox) {
        tags.push(Letterbox);
        frame.push(format!("letterboxed (about {}% of the height in black bars)", pct(a)));
    }
    if let (Some(a), None) = (theirs.pillarbox, yours.pillarbox) {
        tags.push(Pillarbox);
        frame.push(format!("pillarboxed (about {}% of the width in black bars)", pct(a)));
    }
    if !frame.is_empty() {
        lines.push(format!("Frame: theirs is {}.", frame.join(" and ")));
    }

    let (gt, gy) = (theirs.grain, yours.grain);
    if gt - gy >= GRAIN_FLOOR && gt >= GRAIN_RATIO * gy {
        tags.push(MoreGrain);
        lines.push("Grain: theirs is visibly grainier.".to_string());
    } else if gy - gt >= GRAIN_FLOOR && gy >= GRAIN_RATIO * gt {
        tags.push(LessGrain);
        lines.push("Grain: theirs is cleaner; yours is visibly grainier.".to_string());
    }

    if tags.is_empty() {
        lines.push("No measurable difference: the looks already match closely.".to_string());
    }
    lines.truncate(MAX_LINES);
    LookDiff { tags, lines: lines.into_iter().map(clip_line).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 320;
    const H: usize = 180;

    /// A neutral picture: a smooth tonal gradient plus colour patches (skin and balanced
    /// mid-saturation colours).
    fn yours() -> Image {
        let mut img = Image::filled(W, H, [0.0, 0.0, 0.0, 1.0]);
        for y in 0..H {
            for x in 0..W {
                let t = x as f32 / (W - 1) as f32;
                let g = 0.003 + 0.9 * t * t;
                let c = match (x / 40, y / 30) {
                    (0, 1) => [0.45, 0.25, 0.17],
                    (2, 2) => [0.30, 0.10, 0.10],
                    (3, 2) => [0.10, 0.25, 0.10],
                    (4, 2) => [0.10, 0.12, 0.30],
                    (5, 2) => [0.10, 0.28, 0.28],
                    (6, 2) => [0.28, 0.10, 0.28],
                    _ => [g, g, g],
                };
                put(&mut img, x, y, c);
            }
        }
        img
    }

    fn put(img: &mut Image, x: usize, y: usize, c: [f32; 3]) {
        if let Some(p) = img.px.get_mut((y * W + x) * 4..(y * W + x) * 4 + 3) {
            p.copy_from_slice(&c);
        }
    }

    fn map(src: &Image, mut f: impl FnMut(usize, usize, [f32; 3]) -> [f32; 3]) -> Image {
        let mut out = src.clone();
        for y in 0..H {
            for x in 0..W {
                let o = (y * W + x) * 4;
                let c = [src.px[o], src.px[o + 1], src.px[o + 2]];
                put(&mut out, x, y, f(x, y, c));
            }
        }
        out
    }

    fn l(c: [f32; 3]) -> f32 {
        luma(c)
    }

    fn mix(a: [f32; 3], b: [f32; 3], w: f32) -> [f32; 3] {
        [0, 1, 2].map(|i| a[i] * (1.0 - w) + b[i] * w)
    }

    fn tint(c: [f32; 3], shadow: [f32; 3], high: [f32; 3]) -> [f32; 3] {
        let w = (l(c) / 0.4).clamp(0.0, 1.0);
        let m = mix(shadow, high, w);
        [c[0] * m[0], c[1] * m[1], c[2] * m[2]]
    }

    fn facts(img: &Image) -> LookFacts {
        LookFacts::measure(img, WorkingSpace::Rec709).expect("facts")
    }

    fn diff(theirs: &Image) -> LookDiff {
        difference(&facts(theirs), &facts(&yours()))
    }

    #[test]
    fn identical_looks_have_no_tags() {
        let d = diff(&yours());
        assert!(d.tags.is_empty(), "{:?}", d.tags);
        assert!(d.sheet().contains("No measurable difference"), "{}", d.sheet());
    }

    #[test]
    fn teal_and_orange_split_tone() {
        let d = diff(&map(&yours(), |_, _, c| tint(c, [0.7, 1.0, 1.2], [1.2, 1.0, 0.75])));
        for t in [LookTag::TealShadows, LookTag::OrangeHighlights, LookTag::SplitToneTealOrange] {
            assert!(d.tags.contains(&t), "{t:?} missing in {:?}\n{}", d.tags, d.sheet());
        }
        assert!(d.sheet().contains("Shadows: theirs lean teal"), "{}", d.sheet());
    }

    #[test]
    fn warm_highlights_only() {
        let d = diff(&map(&yours(), |_, _, c| tint(c, [1.0; 3], [1.2, 1.0, 0.75])));
        assert!(d.tags.contains(&LookTag::OrangeHighlights), "{:?}", d.tags);
        assert!(!d.tags.contains(&LookTag::TealShadows) && !d.tags.contains(&LookTag::SplitToneTealOrange), "{:?}", d.tags);
    }

    #[test]
    fn faded_blacks_and_the_reverse() {
        let faded = map(&yours(), |_, _, c| c.map(|v| 0.06 + 0.94 * v));
        let d = diff(&faded);
        assert!(d.tags.contains(&LookTag::FadedBlacks), "{:?}\n{}", d.tags, d.sheet());
        assert!(d.tags.contains(&LookTag::LessContrast), "{:?}", d.tags);
        let back = difference(&facts(&yours()), &facts(&faded));
        assert!(back.tags.contains(&LookTag::CrushedBlacks), "{:?}\n{}", back.tags, back.sheet());
    }

    #[test]
    fn letterbox_is_found_and_not_mistaken_for_crushed_blacks() {
        let bar = 23;
        let boxed = map(&yours(), |_, y, c| if y < bar || y >= H - bar { [0.0; 3] } else { c });
        let f = facts(&boxed);
        assert!(f.letterbox.is_some_and(|s| (s - 0.255).abs() < 0.02), "{f:?}");
        assert!(f.pillarbox.is_none());
        let d = difference(&f, &facts(&yours()));
        assert!(d.tags.contains(&LookTag::Letterbox), "{:?}", d.tags);
        assert!(!d.tags.contains(&LookTag::CrushedBlacks) && !d.tags.contains(&LookTag::Darker), "{:?}", d.tags);
        assert!(d.sheet().contains("letterboxed"), "{}", d.sheet());
        // pillarbox
        let pill = map(&yours(), |x, _, c| if !(40..W - 40).contains(&x) { [0.0; 3] } else { c });
        let d = diff(&pill);
        assert!(d.tags.contains(&LookTag::Pillarbox) && !d.tags.contains(&LookTag::Letterbox), "{:?}", d.tags);
    }

    #[test]
    fn heavy_grain() {
        let mut s = 12345u32;
        let grainy = map(&yours(), |_, _, c| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let u = (s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            c.map(|v| v * (1.0 + 0.25 * u))
        });
        let d = diff(&grainy);
        assert!(d.tags.contains(&LookTag::MoreGrain), "{:?}\n{}", d.tags, d.sheet());
        let back = difference(&facts(&yours()), &facts(&grainy));
        assert!(back.tags.contains(&LookTag::LessGrain), "{:?}", back.tags);
    }

    #[test]
    fn exposure_one_stop_each_way() {
        let up = diff(&map(&yours(), |_, _, c| c.map(|v| v * 2.0)));
        assert!(up.tags.contains(&LookTag::Brighter) && !up.tags.contains(&LookTag::Darker), "{:?}", up.tags);
        let down = diff(&map(&yours(), |_, _, c| c.map(|v| v * 0.5)));
        assert!(down.tags.contains(&LookTag::Darker), "{:?}", down.tags);
        assert!(down.sheet().contains("Exposure: theirs is about 1.0 stop darker"), "{}", down.sheet());
    }

    #[test]
    fn desaturated_and_oversaturated() {
        let flat = diff(&map(&yours(), |_, _, c| mix(c, [l(c); 3], 0.5)));
        assert!(flat.tags.contains(&LookTag::LessSaturated), "{:?}\n{}", flat.tags, flat.sheet());
        let back = difference(&facts(&yours()), &facts(&map(&yours(), |_, _, c| mix(c, [l(c); 3], 0.5))));
        assert!(back.tags.contains(&LookTag::MoreSaturated), "{:?}", back.tags);
    }

    #[test]
    fn lines_are_short_and_finite() {
        let d = diff(&map(&yours(), |_, _, c| tint(c, [0.7, 1.0, 1.2], [1.2, 1.0, 0.75]).map(|v| 0.06 + 0.94 * v * 2.0)));
        assert!(d.lines.len() <= MAX_LINES && d.lines.iter().all(|s| s.chars().count() <= MAX_LINE_CHARS), "{:?}", d.lines);
        let s = d.sheet();
        assert!(s.starts_with("Theirs vs yours (measured):") && !s.contains("NaN") && !s.contains("inf"), "{s}");
    }

    #[test]
    fn hostile_frames() {
        assert_eq!(LookFacts::measure(&Image::new(0, 0), WorkingSpace::Rec709), None);
        assert_eq!(LookFacts::measure(&Image::new(8, 8), WorkingSpace::Rec709), None, "fully transparent");
        let short = Image { w: 100, h: 100, px: vec![0.5; 16] };
        assert_eq!(LookFacts::measure(&short, WorkingSpace::Rec709), None);
        let huge = Image { w: usize::MAX, h: 2, px: vec![] };
        assert_eq!(LookFacts::measure(&huge, WorkingSpace::Rec709), None);
        let one = Image::filled(1, 1, [0.2, 0.2, 0.2, 1.0]);
        assert!(LookFacts::measure(&one, WorkingSpace::Rec709).is_some_and(|f| f.black_stops.is_finite()));
        let black = Image::filled(64, 36, [0.0, 0.0, 0.0, 1.0]);
        let fb = LookFacts::measure(&black, WorkingSpace::Rec709);
        if let Some(fb) = &fb {
            assert!(fb.letterbox.is_none() && fb.black_stops.is_finite());
            assert!(!difference(fb, &facts(&yours())).sheet().contains("NaN"));
        }
        let mut evil = yours();
        let vals = [f32::NAN, f32::INFINITY, -f32::INFINITY, f32::MAX, -1e30, 0.0];
        for (i, p) in evil.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if i % 3 == 0 {
                p[i % 4] = vals[i % vals.len()];
            }
        }
        if let Some(fe) = LookFacts::measure(&evil, WorkingSpace::Rec2100Pq) {
            let s = difference(&fe, &facts(&yours())).sheet();
            assert!(!s.contains("NaN") && !s.contains("inf"), "{s}");
        }
    }
}
