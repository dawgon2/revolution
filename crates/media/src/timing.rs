//! Variable frame rate (Y2): how evenly a stream's frames are spaced, from the container's own
//! timestamps. Phones and screen recorders write frames whenever they have one, so the gap
//! between two frames jitters, drops in low light or switches mid-clip; the nominal
//! [`crate::VideoStreamInfo::frame_rate`] is then only the typical rate. Decoding already picks
//! frames by timestamp; this is what Info / Properties shows about it.

use std::sync::Arc;

use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};

/// Most frame times looked at (≈ 19 h at 60 fps); the rest of a longer stream is not read.
pub const MAX_TIMING_FRAMES: usize = 1 << 22;

/// Frame spacing of a variable-frame-rate stream.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameTiming {
    /// Frames per second over the whole stream (gaps ÷ time from the first to the last frame).
    #[serde(rename = "averageFps")]
    pub average_fps: f64,
    /// The longest gap between two neighbouring frames, as a rate.
    #[serde(rename = "minFps")]
    pub min_fps: f64,
    /// The shortest gap between two neighbouring frames, as a rate.
    #[serde(rename = "maxFps")]
    pub max_fps: f64,
    /// The rate most frames are recorded at: the mean of the gaps within 10 % of the median gap
    /// (jitter averages out; drops and switches are left out). 0 in files from before it existed.
    #[serde(rename = "typicalFps", default)]
    pub typical_fps: f64,
    /// How many frames (distinct times) the stream has (at most [`MAX_TIMING_FRAMES`]); 0 in
    /// files from before it existed.
    #[serde(default)]
    pub frames: u64,
}

impl FrameTiming {
    /// The spacing of `pts` (presentation times in any order, `units_per_second` units each),
    /// or `None` when the stream is constant-rate: every gap within one timebase unit or 0.2 %
    /// of the typical gap of each other (a 1/1000 s timebase writes 29.97 fps as 33 / 34 ms
    /// gaps; that is still constant). Repeated times are ignored; fewer than three gaps or a
    /// bad timebase give `None`.
    pub fn detect(pts: impl IntoIterator<Item = i64>, units_per_second: f64) -> Option<FrameTiming> {
        if !(units_per_second.is_finite() && units_per_second > 0.0) {
            return None;
        }
        let mut times: Vec<i64> = pts.into_iter().take(MAX_TIMING_FRAMES).collect();
        times.sort_unstable();
        let mut gaps: Vec<u64> = times.windows(2).filter_map(|w| w[1].checked_sub(w[0])).filter(|g| *g > 0).map(|g| g as u64).collect();
        if gaps.len() < 3 {
            return None;
        }
        gaps.sort_unstable();
        let (min_gap, max_gap) = (*gaps.first()?, *gaps.last()?);
        let typical = *gaps.get(gaps.len() / 2)?;
        if max_gap - min_gap <= (typical / 500).max(1) {
            return None;
        }
        let span = times.last()?.checked_sub(*times.first()?)?;
        if span <= 0 {
            return None;
        }
        let rate = |gap: f64| units_per_second / gap;
        let near: Vec<u64> = gaps.iter().copied().filter(|g| g.abs_diff(typical) <= typical / 10).collect();
        let near_mean = near.iter().map(|g| *g as f64).sum::<f64>() / near.len().max(1) as f64;
        let t = FrameTiming {
            average_fps: rate(span as f64 / gaps.len() as f64),
            min_fps: rate(max_gap as f64),
            max_fps: rate(min_gap as f64),
            typical_fps: rate(near_mean),
            frames: gaps.len() as u64 + 1,
        };
        // project files store these as JSON numbers, which can't hold infinities
        [t.average_fps, t.min_fps, t.max_fps, t.typical_fps].iter().all(|f| f.is_finite() && *f > 0.0).then_some(t)
    }

    /// The clip's nominal rate: [`Self::typical_fps`] as the closest standard rate within 0.2 %
    /// (a ±1 ms jitter around 33.3 ms is 30 fps), else as it is. None when it isn't known.
    pub fn typical_rate(&self) -> Option<FrameRate> {
        let fps = self.typical_fps;
        if !(fps.is_finite() && fps > 0.0) {
            return None;
        }
        let close = FrameRate::COMMON.into_iter().map(|r| (r, (r.as_f64() - fps).abs())).filter(|(_, d)| *d <= fps * 0.002);
        Some(close.min_by(|a, b| a.1.total_cmp(&b.1)).map_or_else(|| FrameRate::from_f64(fps), |(r, _)| r)).filter(|r| r.num > 0 && r.den > 0)
    }

    /// "Variable, 29.85 fps average (15–30)" style text for Info / Properties.
    pub fn label(&self) -> String {
        let r = Self::rate;
        format!("Variable, {} fps average ({}–{})", r(self.average_fps), r(self.min_fps), r(self.max_fps))
    }

    /// "29.85 fps (15–30)": the average and the range, without words (translated UIs put
    /// their own label beside it).
    pub fn numbers(&self) -> String {
        let r = Self::rate;
        format!("{} fps ({}–{})", r(self.average_fps), r(self.min_fps), r(self.max_fps))
    }

    fn rate(f: f64) -> String {
        let s = format!("{f:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Where a source's frames start in media time: the nominal grid of a frame rate, or (for a
/// variable-frame-rate source) the container's own frame times. Render code that needs "the
/// frame at t", "where does it start" or "the next frame" asks this instead of the frame rate.
pub trait FrameTimes {
    /// Index of the frame shown at media time `t` (the last one starting at or before `t`).
    fn frame_at(&self, t: Tick) -> i64;
    /// Media time at which frame `f` starts.
    fn tick_of(&self, f: i64) -> Tick;
    /// False when there is no usable spacing (a zero or broken frame rate).
    fn usable(&self) -> bool;
}

impl FrameTimes for FrameRate {
    fn frame_at(&self, t: Tick) -> i64 {
        FrameRate::frame_at(*self, t)
    }
    fn tick_of(&self, f: i64) -> Tick {
        FrameRate::tick_of(*self, f)
    }
    fn usable(&self) -> bool {
        self.frame_duration().0 > 0
    }
}

impl<T: FrameTimes + ?Sized> FrameTimes for &T {
    fn frame_at(&self, t: Tick) -> i64 {
        (**self).frame_at(t)
    }
    fn tick_of(&self, f: i64) -> Tick {
        (**self).tick_of(f)
    }
    fn usable(&self) -> bool {
        (**self).usable()
    }
}

/// A source's frame clock ([`FrameTimes`]).
#[derive(Clone, Debug, PartialEq)]
pub enum FrameClock {
    /// Constant rate: frame `f` starts at `f` frame durations.
    Grid(FrameRate),
    /// The start time of every frame, sorted, without repeats, then the end of the last frame
    /// (at least two entries). Times before the first frame or after the end continue with the
    /// first / last gap.
    Times(Arc<[Tick]>),
}

impl FrameClock {
    /// The clock of frames starting at `starts` (any order; at most [`MAX_TIMING_FRAMES`]), the
    /// last one lasting as long as the gap before it; `fallback`'s grid when fewer than two
    /// distinct times are left.
    pub fn from_starts(starts: impl IntoIterator<Item = Tick>, fallback: FrameRate) -> FrameClock {
        let mut t: Vec<Tick> = starts.into_iter().take(MAX_TIMING_FRAMES).collect();
        t.sort_unstable();
        t.dedup();
        let end = match t.as_slice() {
            [.., a, b] => b.0.checked_add(b.0.saturating_sub(a.0)).map(Tick),
            _ => None,
        };
        match end {
            Some(e) if t.last().is_some_and(|l| e > *l) => {
                t.push(e);
                FrameClock::Times(t.into())
            }
            _ => FrameClock::Grid(fallback),
        }
    }
}

impl FrameTimes for FrameClock {
    fn frame_at(&self, t: Tick) -> i64 {
        match self {
            FrameClock::Grid(r) => FrameTimes::frame_at(r, t),
            FrameClock::Times(v) => {
                let k = v.partition_point(|s| *s <= t);
                // before the first frame: whole first gaps back from it (floor)
                if k == 0 {
                    let (Some(a), Some(b)) = (v.first(), v.get(1)) else { return 0 };
                    let gap = b.0.saturating_sub(a.0).max(1);
                    let back = a.0.saturating_sub(t.0);
                    return -((back.saturating_add(gap - 1)) / gap);
                }
                let last = v.len() - 1;
                if k - 1 < last {
                    return (k - 1) as i64;
                }
                // at or past the end: the end entry, then whole last gaps after it
                let (Some(a), Some(b)) = (v.get(last.wrapping_sub(1)), v.get(last)) else { return last as i64 };
                let gap = b.0.saturating_sub(a.0).max(1);
                (last as i64).saturating_add(t.0.saturating_sub(b.0) / gap)
            }
        }
    }
    fn tick_of(&self, f: i64) -> Tick {
        match self {
            FrameClock::Grid(r) => FrameTimes::tick_of(r, f),
            FrameClock::Times(v) => {
                let last = v.len().saturating_sub(1);
                if let Some(t) = usize::try_from(f).ok().and_then(|i| v.get(i)) {
                    return *t;
                }
                let (first, end) = (v.first().copied().unwrap_or(Tick::ZERO), v.get(last).copied().unwrap_or(Tick::ZERO));
                if f < 0 {
                    let gap = v.get(1).map_or(1, |b| b.0.saturating_sub(first.0).max(1));
                    Tick(first.0.saturating_add(gap.saturating_mul(f)))
                } else {
                    let gap = v.get(last.wrapping_sub(1)).map_or(1, |a| end.0.saturating_sub(a.0).max(1));
                    Tick(end.0.saturating_add(gap.saturating_mul(f.saturating_sub(last as i64))))
                }
            }
        }
    }
    fn usable(&self) -> bool {
        match self {
            FrameClock::Grid(r) => FrameTimes::usable(r),
            FrameClock::Times(v) => v.len() >= 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_rate_streams_are_not_variable() {
        // 29.97 at 1/30000: every gap 1001
        assert_eq!(FrameTiming::detect((0..300).map(|i| i * 1001), 30000.0), None);
        // 29.97 at 1/1000: 33 / 34 ms rounding
        assert_eq!(FrameTiming::detect((0..300).map(|i| i * 1001 / 30), 1000.0), None);
        // B-frame order doesn't matter
        assert_eq!(FrameTiming::detect([0, 3003, 1001, 2002, 6006, 4004, 5005], 30000.0), None);
    }

    #[test]
    fn jitter_drops_and_switches_are_variable() {
        // ±1 ms jitter around 33.3 ms at 1/1000
        let mut t = 0;
        let jitter: Vec<i64> = (0..300)
            .map(|i| {
                let v = t;
                t += [33, 32, 34, 33, 35, 31][i % 6];
                v
            })
            .collect();
        let j = FrameTiming::detect(jitter, 1000.0).expect("jitter is variable");
        // (≈ 33 ms is 30.3 fps: not within 0.2 % of a standard rate)
        assert_eq!(j.typical_rate(), Some(FrameRate::new(30297, 1000)), "{j:?}");
        let thirty = (0..300i64).map(|i| i * 3000 + [0, 90, -90][(i % 3) as usize]);
        assert_eq!(FrameTiming::detect(thirty, 90000.0).and_then(|t| t.typical_rate()), Some(FrameRate::FPS_30));
        // the pattern averages 33 ms
        assert!((j.average_fps - 1000.0 / 33.0).abs() < 0.01, "{j:?}");
        assert_eq!(j.frames, 300);
        assert!(j.min_fps < j.average_fps && j.max_fps > j.average_fps);
        // 60 → 30 switch at 1/90000
        let switch = (0..120).map(|i| i * 1500).chain((0..120).map(|i| 120 * 1500 + i * 3000));
        let s = FrameTiming::detect(switch, 90000.0).expect("switch is variable");
        assert!((s.min_fps - 30.0).abs() < 1e-9 && (s.max_fps - 60.0).abs() < 1e-9, "{s:?}");
        assert_eq!(s.typical_rate(), Some(FrameRate::FPS_60), "120 gaps at 60 fps, 119 at 30");
        // 29.97 with jitter stays 29.97, not 30
        let ntsc = (0..300i64).map(|i| i * 3003 + [0, 90, -90][(i % 3) as usize]);
        assert_eq!(FrameTiming::detect(ntsc, 90000.0).and_then(|t| t.typical_rate()), Some(FrameRate::FPS_29_97));
        assert_eq!(s.label(), "Variable, 40.06 fps average (30–60)");
        assert_eq!(s.numbers(), "40.06 fps (30–60)");
    }

    #[test]
    fn hostile_times_do_not_panic() {
        assert_eq!(FrameTiming::detect([], 1000.0), None);
        assert_eq!(FrameTiming::detect([i64::MIN, 0, i64::MAX, 5], 1000.0), None);
        assert_eq!(FrameTiming::detect([0, 10, 20, 40], 0.0), None);
        assert_eq!(FrameTiming::detect([0, 10, 20, 40], f64::NAN), None);
        assert_eq!(FrameTiming::detect([5, 5, 5, 5, 5], 1000.0), None);
        let _ = FrameTiming::detect([i64::MIN, i64::MIN + 1, 0, 1, i64::MAX], 1e-300);
        assert_eq!(FrameTiming::detect([0, 1, 3, 100], 1e308 * 10.0), None);
        assert!(FrameTiming::detect([0, 1, 3, 100, 101], 1e-320).is_none_or(|t| t.min_fps.is_finite()));
        let odd = FrameTiming { average_fps: 1.0, min_fps: 1.0, max_fps: 1.0, typical_fps: f64::NAN, frames: 3 };
        assert_eq!(odd.typical_rate(), None);
        assert_eq!(FrameTiming { typical_fps: 1e-300, ..odd }.typical_rate(), None);
    }

    fn ms(v: &[i64]) -> FrameClock {
        FrameClock::from_starts(v.iter().map(|m| Tick::from_rational(*m, 1, 1000)), FrameRate::new(30, 1))
    }

    #[test]
    fn a_clock_of_times_finds_the_frame_covering_a_time() {
        let c = ms(&[0, 33, 67, 133, 167]); // a 66 ms gap after frame 2
        let at = |m: i64| FrameTimes::frame_at(&c, Tick::from_rational(m, 1, 1000));
        assert_eq!([at(0), at(32), at(33), at(66), at(67), at(132), at(133)], [0, 0, 1, 1, 2, 2, 3]);
        assert_eq!(FrameTimes::tick_of(&c, 3), Tick::from_rational(133, 1, 1000));
        // the last frame lasts as long as the gap before it; past the end continues with it
        assert_eq!(FrameTimes::tick_of(&c, 5), Tick::from_rational(201, 1, 1000));
        assert_eq!(at(200), 4);
        assert_eq!(at(201), 5);
        assert_eq!(at(236), 6);
        assert_eq!(FrameTimes::tick_of(&c, 6), Tick::from_rational(235, 1, 1000));
        // before the first frame: whole first gaps back
        let c2 = ms(&[100, 133, 167]);
        let at2 = |m: i64| FrameTimes::frame_at(&c2, Tick::from_rational(m, 1, 1000));
        assert_eq!([at2(99), at2(67), at2(66)], [-1, -1, -2]);
        assert_eq!(FrameTimes::tick_of(&c2, -1), Tick::from_rational(67, 1, 1000));
        // frame_at(tick_of(f)) == f inside the clip
        for f in 0..5 {
            assert_eq!(FrameTimes::frame_at(&c, FrameTimes::tick_of(&c, f)), f);
        }
        assert!(c.usable());
    }

    #[test]
    fn too_few_times_fall_back_to_the_grid() {
        let r = FrameRate::new(25, 1);
        assert_eq!(FrameClock::from_starts([], r), FrameClock::Grid(r));
        assert_eq!(FrameClock::from_starts([Tick(5), Tick(5)], r), FrameClock::Grid(r));
        // an end that would overflow
        assert_eq!(FrameClock::from_starts([Tick(0), Tick(i64::MAX)], r), FrameClock::Grid(r));
        let c = FrameClock::from_starts([Tick(i64::MIN), Tick(-1), Tick(0)], r);
        for t in [i64::MIN, -5, 0, 7, i64::MAX] {
            let f = FrameTimes::frame_at(&c, Tick(t));
            let _ = FrameTimes::tick_of(&c, f);
        }
        for f in [i64::MIN, -3, 0, 3, i64::MAX] {
            let _ = FrameTimes::tick_of(&c, f);
        }
    }
}
