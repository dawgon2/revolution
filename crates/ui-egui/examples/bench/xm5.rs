//! The fork's Fujifilm X-M5 6.2K HEVC scenarios (`FORK-PLAN.md` §4; results in
//! `docs/fork-benchmarks.md`): `cargo xtask bench --sections xm5 [--only play-full] [--quick]`.
//!
//! Clips come from `$FILMCRAFT_XM5_DIR` (real camera clips linked under the names below) or
//! `target/fixtures/xm5/`, where missing ones are generated with ffmpeg as synthetic stand-ins
//! (6240×4160 HEVC Main10 long GOP, ~200 Mbit/s, MOV, 24-bit LPCM; slow: ~6 fps with libx265).
//! Every row also carries what the GPU did meanwhile (AMD `gpu_busy_percent`, VRAM in use) and
//! the process's peak resident memory, sampled every 100 ms.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use filmcraft_engine::Session;
use filmcraft_project::{ItemId, ParamValue, Project, SequenceSettings, TrackKind, find_effect, resolve_auto_points};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

use crate::Opts;
use crate::playback::{self, Display, PlayReport, load_avg, ms, pct};
use crate::sections::new_bench;

/// A synthetic stand-in: file name, frame rate (num, den), seconds, pixel format, x265 profile,
/// lavfi picture source (`{S}` = size, `{R}` = rate), keyframe interval.
struct Spec {
    name: &'static str,
    rate: (u32, u32),
    secs: u32,
    pix_fmt: &'static str,
    profile: &'static str,
    src: &'static str,
    keyint: u32,
}

pub const PLAY: &str = "xm5_420_2997_60s.mov";
pub const P2398: &str = "xm5_420_2398_60s.mov";
pub const C422: &str = "xm5_422_2997_20s.mov";
pub const CLIP_B: &str = "xm5_420_2997_30s_b.mov";
pub const LONG: &str = "xm5_420_2997_5min.mov";

const SPECS: &[Spec] = &[
    Spec {
        name: PLAY,
        rate: (30000, 1001),
        secs: 60,
        pix_fmt: "yuv420p10le",
        profile: "main10",
        src: "testsrc2=s={S}:r={R},noise=alls=14:allf=t+u",
        keyint: 30,
    },
    Spec {
        name: P2398,
        rate: (24000, 1001),
        secs: 60,
        pix_fmt: "yuv420p10le",
        profile: "main10",
        src: "testsrc2=s={S}:r={R},hue=h=t*30,noise=alls=14:allf=t+u",
        keyint: 24,
    },
    Spec {
        name: C422,
        rate: (30000, 1001),
        secs: 20,
        pix_fmt: "yuv422p10le",
        profile: "main422-10",
        src: "testsrc2=s={S}:r={R},hue=h=t*60,noise=alls=14:allf=t+u",
        keyint: 30,
    },
    Spec {
        name: CLIP_B,
        rate: (30000, 1001),
        secs: 30,
        pix_fmt: "yuv420p10le",
        profile: "main10",
        src: "testsrc=s={S}:r={R},noise=alls=14:allf=t+u",
        keyint: 30,
    },
    Spec {
        name: LONG,
        rate: (30000, 1001),
        secs: 300,
        pix_fmt: "yuv420p10le",
        profile: "main10",
        src: "testsrc2=s={S}:r={R},hue=h=t*10,noise=alls=14:allf=t+u",
        keyint: 30,
    },
];

/// Where the clips live: `$FILMCRAFT_XM5_DIR`, else `<repo>/target/fixtures/xm5`.
pub fn xm5_dir() -> PathBuf {
    match std::env::var_os("FILMCRAFT_XM5_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/xm5"),
    }
}

/// The clip `name`, generating the synthetic stand-in on first use (None when ffmpeg or libx265
/// is missing).
pub fn clip(name: &str) -> Option<PathBuf> {
    let dir = xm5_dir();
    let path = dir.join(name);
    if path.exists() {
        return Some(path);
    }
    let spec = SPECS.iter().find(|s| s.name == name)?;
    let ff = crate::fixtures::ffmpeg()?;
    std::fs::create_dir_all(&dir).ok()?;
    eprintln!("generating {} (6.2K x265: this takes a while)", path.display());
    let rate = format!("{}/{}", spec.rate.0, spec.rate.1);
    let src = spec.src.replace("{S}", "6240x4160").replace("{R}", &rate);
    // two frame threads, a short lookahead, a two-frame input queue and no `-shortest` (its sync
    // queue holds raw frames; `-t` already ends every stream) keep ffmpeg near 6.6 GB at
    // 6.2K 4:2:2; its defaults grew past 10 GB and the OOM killer took the session on a 16 GB laptop
    let x265 = format!(
        "keyint={k}:min-keyint={k}:bitrate=200000:vbv-maxrate=200000:vbv-bufsize=400000:log-level=error:frame-threads=2:rc-lookahead=5:lookahead-slices=0",
        k = spec.keyint
    );
    let tmp = dir.join(format!(".{name}.{}.tmp.mov", std::process::id()));
    let ok = Command::new(ff)
        .args([
            "-v",
            "error",
            "-y",
            "-thread_queue_size",
            "2",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:r=48000,aformat=channel_layouts=stereo",
        ])
        .args(["-t", &spec.secs.to_string(), "-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", spec.pix_fmt, "-profile:v", spec.profile])
        .args(["-x265-params", &x265, "-color_range", "tv", "-tag:v", "hvc1", "-c:a", "pcm_s24le"])
        .arg(&tmp)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("  ffmpeg failed for {name}");
        return None;
    }
    std::fs::rename(&tmp, &path).ok()?;
    Some(path)
}

// ------------------------------------------------------------------------------------ sampling

/// The AMD GPU's sysfs device directory (`/sys/class/drm/cardN/device` with `gpu_busy_percent`).
fn amd_gpu_sysfs() -> Option<PathBuf> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir("/sys/class/drm").ok()?.flatten().map(|e| e.path()).collect();
    cards.sort();
    cards
        .into_iter()
        .map(|c| c.join("device"))
        .find(|d| std::fs::read_to_string(d.join("vendor")).is_ok_and(|v| v.trim() == "0x1002") && d.join("gpu_busy_percent").exists())
}

fn read_num(p: &Path) -> Option<f64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

/// Resident set size of this process (bytes).
fn rss_bytes() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: f64 = s.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096.0)
}

#[derive(Default, Clone)]
pub struct Sampled {
    pub gpu_busy_mean: f64,
    pub gpu_busy_max: f64,
    pub vram_max_mb: f64,
    pub vram_start_mb: f64,
    pub rss_max_mb: f64,
    pub rss_start_mb: f64,
    pub samples: usize,
}

impl Sampled {
    fn json(&self) -> Value {
        json!({
            "gpu_busy_pct_mean": self.gpu_busy_mean, "gpu_busy_pct_max": self.gpu_busy_max,
            "vram_mb_max": self.vram_max_mb, "vram_mb_start": self.vram_start_mb,
            "rss_mb_max": self.rss_max_mb, "rss_mb_start": self.rss_start_mb,
        })
    }
}

/// Samples GPU busy %, VRAM and RSS every 100 ms on a thread until dropped / finished.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Sampled>>,
}

impl Sampler {
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::spawn(move || {
            let dev = amd_gpu_sysfs();
            let vram = |d: &Option<PathBuf>| d.as_ref().and_then(|d| read_num(&d.join("mem_info_vram_used"))).unwrap_or(0.0) / 1e6;
            let mut out = Sampled { vram_start_mb: vram(&dev), rss_start_mb: rss_bytes().unwrap_or(0.0) / 1e6, ..Default::default() };
            let mut busy_sum = 0.0;
            while !s2.load(Ordering::Relaxed) {
                if let Some(d) = &dev {
                    let b = read_num(&d.join("gpu_busy_percent")).unwrap_or(0.0);
                    busy_sum += b;
                    out.gpu_busy_max = out.gpu_busy_max.max(b);
                }
                out.vram_max_mb = out.vram_max_mb.max(vram(&dev));
                out.rss_max_mb = out.rss_max_mb.max(rss_bytes().unwrap_or(0.0) / 1e6);
                out.samples += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            out.gpu_busy_mean = busy_sum / out.samples.max(1) as f64;
            out
        });
        Self { stop, handle: Some(handle) }
    }

    pub fn finish(mut self) -> Sampled {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().and_then(|h| h.join().ok()).unwrap_or_default()
    }
}

// ------------------------------------------------------------------------------------ sequences

const FPS_29_97: FrameRate = FrameRate::FPS_29_97;

/// Fill scale (%) of a 6240×4160 clip on a `w`×`h` frame (Scale to Frame Fill).
fn fill_scale(w: u32, h: u32) -> f64 {
    100.0 * (w as f64 / 6240.0).max(h as f64 / 4160.0)
}

/// A Lumetri grade with Basic, Curves, HSL Secondary and Vignette in use, plus Sharpen, on every
/// clip of V1. (F-Log2 → Rec.709 conversion arrives in Phase 4; until then this is the grade.)
/// Interpret the media of `seq`'s V1 clips as F-Log2 / F-Gamut (the X-M5's log profile; the
/// synthetic clips are not log-encoded, but the conversion costs the same).
fn interpret_flog2(s: &mut Session, seq: ItemId) {
    s.edit("interpret", |p: &mut Project, _| {
        let media: Vec<ItemId> = p.sequence(seq).map(|q| q.video_tracks[0].items.iter().map(|it| it.item).collect()).unwrap_or_default();
        for id in media {
            if let Some(filmcraft_project::ItemKind::Media(m)) = p.item_mut(id).map(|i| &mut i.kind) {
                m.interpret.color_space = Some(filmcraft_color::ColorSpace::FLog2FGamut);
            }
        }
        Ok(())
    })
    .expect("interpret");
}

/// Make `seq` a DaVinci Wide Gamut / Intermediate sequence (the fork's grading space).
fn set_dwg(s: &mut Session, seq: ItemId) {
    s.edit("dwg", |p: &mut Project, _| {
        if let Some(q) = p.sequence_mut(seq) {
            q.settings.color.working = filmcraft_color::WorkingSpace::DaVinciWide;
            q.settings.working_space = filmcraft_color::WorkingSpace::DaVinciWide.label().into();
        }
        Ok(())
    })
    .expect("dwg");
}

/// A grade node with spatial noise reduction (luma 0.3, chroma 0.3, σ 1.5 source px) on every
/// clip of the first video track (fork G2.4 acceptance: play-graded with NR on).
fn add_nr_node(s: &mut Session, seq: ItemId) {
    s.edit("nr", |p: &mut Project, _| {
        let q = p.sequence_mut(seq).expect("seq");
        for it in &mut q.video_tracks[0].items {
            let mut n = filmcraft_project::grade::GradeNode::new(1);
            for (k, v) in [("nr.luma", 0.3), ("nr.chroma", 0.3), ("nr.size", 1.5)] {
                n.params.insert(k.into(), filmcraft_project::Param::new(ParamValue::Float(v)));
            }
            it.grade = Some(filmcraft_project::grade::Grade { nodes: vec![n], bypass: false });
        }
        Ok(())
    })
    .expect("nr");
}

/// Clip temporal noise reduction (luma 0.5, chroma 0.5, ±1 frame, motion 0.5) on every clip of
/// the first video track (fork 6.7 / 6.7b: `export-tnr`, `cache-tnr`).
fn add_tnr_clip(s: &mut Session, seq: ItemId) {
    s.edit("tnr", |p: &mut Project, _| {
        let q = p.sequence_mut(seq).expect("seq");
        for it in &mut q.video_tracks[0].items {
            let mut n = filmcraft_project::TemporalNr::default();
            for (k, v) in [("luma", 0.5), ("chroma", 0.5), ("frames", 1.0), ("motion", 0.5)] {
                n.params.insert(k.into(), filmcraft_project::Param::new(ParamValue::Float(v)));
            }
            it.temporal_nr = Some(n);
        }
        Ok(())
    })
    .expect("tnr");
}

/// Whether `--only` names a temporal-NR case (slow: only when asked for by name).
fn asks_tnr(o: &Opts, name: &str) -> bool {
    o.only.as_deref().is_some_and(|x| x.contains("tnr") && name.contains(x))
}

fn grade(s: &mut Session, seq: ItemId) {
    interpret_flog2(s, seq);
    s.edit("grade", |p: &mut Project, _| {
        let q = p.sequence_mut(seq).expect("seq");
        for it in &mut q.video_tracks[0].items {
            let mut l = find_effect("lumetri").expect("lumetri").instance();
            let set: [(&str, ParamValue); 9] = [
                ("temperature", ParamValue::Float(12.0)),
                ("exposure", ParamValue::Float(0.3)),
                ("contrast", ParamValue::Float(20.0)),
                ("saturation", ParamValue::Float(115.0)),
                ("curve_luma", ParamValue::Curve(vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.82], [1.0, 1.0]])),
                ("hsl_on", ParamValue::Bool(true)),
                ("hsl_hue", ParamValue::Float(30.0)),
                ("hsl_sat", ParamValue::Float(130.0)),
                ("vignette_amount", ParamValue::Float(-1.5)),
            ];
            for (k, v) in set {
                if let Some(x) = l.params.get_mut(k) {
                    x.value = v;
                }
            }
            let mut sh = find_effect("sharpen").expect("sharpen").instance();
            if let Some(x) = sh.params.get_mut("amount") {
                x.value = ParamValue::Float(40.0);
            }
            it.effects.insert(0, l);
            it.effects.insert(1, sh);
        }
        Ok(())
    })
    .expect("grade");
}

/// V1 holds `cuts` (clip, source in-point s, length s) back to back on a `w`×`h` 29.97 sequence,
/// each scaled to fill the frame. Audio of the first cut's clip on A1 when `audio`.
fn cuts_sequence(s: &mut Session, w: u32, h: u32, cuts: &[(ItemId, f64, f64)], audio: bool) -> ItemId {
    let rate = FPS_29_97;
    let scale = fill_scale(w, h);
    s.edit("xm5 sequence", |p: &mut Project, st| {
        let settings = SequenceSettings { width: w, height: h, frame_rate: rate, ..SequenceSettings::default() };
        let seq = p.new_sequence("XM5", settings, 1, 1, None);
        let mut at = Tick::ZERO;
        let (mut vs, mut aus) = (Vec::new(), Vec::new());
        for &(item, src_in, len) in cuts {
            let range = TimeRange::new(rate.snap(Tick::from_seconds_f64(src_in)), rate.snap(Tick::from_seconds_f64(len)));
            let mut v = p.make_track_item(item, TrackKind::Video, at, range, rate).expect("track item");
            for e in &mut v.effects {
                resolve_auto_points(e, (w, h), (6240, 4160));
            }
            if let Some(x) = v.effect_mut("motion").and_then(|m| m.params.get_mut("scale")) {
                x.value = ParamValue::Float(scale);
            }
            if audio && let Some(a) = p.make_track_item(item, TrackKind::Audio, at, range, rate) {
                aus.push(a);
            }
            at += range.duration;
            vs.push(v);
        }
        let q = p.sequence_mut(seq).expect("seq");
        q.video_tracks[0].items = vs;
        q.audio_tracks[0].items = aus;
        st.active_sequence = Some(seq);
        st.open_sequences = vec![seq];
        st.playheads.insert(seq, Tick::ZERO);
        Ok(seq)
    })
    .expect("sequence")
}

fn import(s: &mut Session, name: &str) -> Option<ItemId> {
    let path = clip(name)?;
    Some(playback::import(s, &path))
}

// ------------------------------------------------------------------------------------ section

fn play_row(name: &str, r: &PlayReport, fps: f64, smp: &Sampled, adapter: &str) -> Value {
    let frames = (r.shown + r.dropped).max(1) as f64;
    let mut v = playback::to_json(r, name, "full", if adapter.is_empty() { "cpu" } else { "gpu" });
    v["case"] = json!(name);
    v["adapter"] = json!(adapter);
    v["drop_pct"] = json!(100.0 * r.dropped as f64 / frames);
    v["dropped_per_s"] = json!(r.dropped as f64 / r.seconds.max(1e-9));
    v["cores_needed"] = json!(r.process_cpu / frames * fps / 1000.0);
    v["hw_frames"] = json!(filmcraft_codecs::hw::hw_stats().frames);
    v["sampled"] = smp.json();
    #[cfg(target_os = "linux")]
    {
        v["va_surfaces"] = json!(filmcraft_platform::va::live_surfaces());
    }
    v["decode_vram_mb"] = json!(filmcraft_codecs::vram_usage().0 as f64 / 1e6);
    v["load"] = json!(r.load);
    v["steps"] = json!(r.steps);
    v["gap_ms"] = json!({"p50": playback::pct(&r.gaps, 0.5), "p95": playback::pct(&r.gaps, 0.95), "max": r.gaps.iter().copied().fold(0.0, f64::max)});
    if r.steps.iter().any(|s| *s > 1) || r.label.contains("jkl") {
        eprintln!(
            "xm5 {name}: steps {:?}, new picture every p50 {:.0} / p95 {:.0} / max {:.0} ms ({} pictures)",
            r.steps,
            playback::pct(&r.gaps, 0.5),
            playback::pct(&r.gaps, 0.95),
            r.gaps.iter().copied().fold(0.0, f64::max),
            r.gaps.len() + 1
        );
    }
    eprintln!(
        "xm5 {name}: {}/{} shown/dropped, CPU {:.1} ms/frame, GPU {:.0} %, VRAM {:.0} MB, RSS {:.0} MB, VA surfaces {}, cache VRAM {:.0} MB, GOP seeks {} decoded {} skipped {} evicted {} (load {})",
        r.shown,
        r.dropped,
        r.process_cpu / frames,
        smp.gpu_busy_mean,
        smp.vram_max_mb,
        smp.rss_max_mb,
        v["va_surfaces"],
        filmcraft_codecs::vram_usage().0 as f64 / 1e6,
        r.gop.seeks,
        r.gop.decoded,
        r.gop.skipped,
        r.gop.evicted,
        r.load
    );
    v
}

/// Play `secs` of `seq` at `speed` from `start` (Program monitor at Full, GPU path).
fn play(s: &Session, seq: ItemId, o: &Opts, name: &str, secs: f64, speed: f64, start: Tick, rows: &mut Vec<Value>) {
    play_after(s, seq, o, name, secs, speed, start, None, rows);
}

/// [`play`], first playing 3 s at the same speed from `warm_from` (another part of the timeline)
/// with the same frame server: as the app plays a speed it has played before (it remembers the
/// skimming step per speed).
#[allow(clippy::too_many_arguments)]
fn play_after(s: &Session, seq: ItemId, o: &Opts, name: &str, secs: f64, speed: f64, start: Tick, warm_from: Option<Tick>, rows: &mut Vec<Value>) {
    let fps = s.project.sequence(seq).map(|q| q.settings.frame_rate.as_f64()).unwrap_or(29.97);
    let mut bench = new_bench(s, secs);
    bench.speed = speed;
    let mut display = Display::new(o.gpu);
    if let Some(from) = warm_from {
        bench.seconds = 3.0;
        let _ = bench.play(s, seq, &mut display, 1.0, from, "warm-up");
        bench.server.stop_prefetch();
        bench.seconds = secs;
    }
    let smp = Sampler::start();
    let r = bench.play(s, seq, &mut display, 1.0, start, name);
    let smp = smp.finish();
    rows.push(play_row(name, &r, fps, &smp, &display.adapter));
}

/// [`play`] of a cuts sequence, adding per-cut drop counts: `cut_starts` are the sequence frames
/// where each cut begins (ascending). A dropped frame is a due frame never on screen while due.
fn play_cuts(s: &Session, seq: ItemId, o: &Opts, name: &str, secs: f64, cut_starts: &[i64], rows: &mut Vec<Value>) {
    let fps = s.project.sequence(seq).map(|q| q.settings.frame_rate.as_f64()).unwrap_or(29.97);
    let bench = new_bench(s, secs);
    let mut display = Display::new(o.gpu);
    let smp = Sampler::start();
    let r = bench.play(s, seq, &mut display, 1.0, Tick::ZERO, name);
    let smp = smp.finish();
    let mut row = play_row(name, &r, fps, &smp, &display.adapter);
    // missed frames per cut, and how far into its cut each one was (frames after the cut)
    let mut per_cut = vec![0u32; cut_starts.len()];
    let mut offsets = Vec::new();
    for &f in &r.missed {
        let Some(i) = cut_starts.iter().rposition(|&c| c <= f) else { continue };
        if let Some(n) = per_cut.get_mut(i) {
            *n += 1;
        }
        offsets.push(f - cut_starts.get(i).copied().unwrap_or(0));
    }
    // only the cuts the play actually reached
    let played = rate_frames(fps, r.seconds);
    let reached = cut_starts.iter().filter(|&&c| c < played).count().max(1);
    per_cut.truncate(reached);
    let within = |lo: i64, hi: i64| offsets.iter().filter(|&&x| x >= lo && x < hi).count();
    let cuts_with_drops = per_cut.iter().filter(|&&n| n > 0).count();
    eprintln!(
        "xm5 {name}: {} missed frames over {reached} cuts ({cuts_with_drops} cuts drop), in cut frames 0-14: {}, 15-29: {}, 30+: {}; per cut {per_cut:?}",
        r.missed.len(),
        within(0, 15),
        within(15, 30),
        within(30, i64::MAX)
    );
    row["missed"] = json!(r.missed.len());
    row["cuts"] = json!(reached);
    row["cuts_with_drops"] = json!(cuts_with_drops);
    row["missed_per_cut"] = json!(per_cut);
    row["missed_at_cut_offset"] = json!({"0-14": within(0, 15), "15-29": within(15, 30), "30+": within(30, i64::MAX)});
    rows.push(row);
}

fn rate_frames(fps: f64, secs: f64) -> i64 {
    (fps * secs).floor() as i64
}

pub fn xm5(o: &Opts) -> Vec<Value> {
    // (fork C1) the comp soak: only by its own name, nothing else in this section
    if o.only.as_deref().is_some_and(|x| x.contains("comp-soak")) {
        return comp_soak(o);
    }
    let mut rows = Vec::new();
    let play_s = if o.quick { 8.0 } else { 60.0 };
    if o.wants("ceilings") {
        rows.extend(ceilings(o));
    }
    if o.wants("decode") {
        rows.extend(decode(o));
    }
    if o.only.as_deref().is_some_and(|x| x.contains("open")) {
        // cold source open + first frame per clip (what a cut to a clip not used yet costs)
        let mut s = Session::default();
        for name in [PLAY, P2398, CLIP_B, LONG] {
            let Some(id) = import(&mut s, name) else { continue };
            let t0 = Instant::now();
            let src = s.media.source_for(&s.project, id, s.services.as_ref());
            let opened = ms(t0.elapsed());
            let t1 = Instant::now();
            let ok =
                src.as_ref().map(|x| x.video_frame(filmcraft_media::FrameRequest::full(Tick::from_seconds_f64(1.0))).map(|_| ()).map_err(|e| e.to_string()));
            eprintln!("open {name}: source {opened:.0} ms, first frame {:.0} ms ({ok:?})", ms(t1.elapsed()));
        }
    }
    // (`--only jkl` runs all four JKL cases, `--only "jkl J 1x"` one of them)
    // (name, speed, from (s), warm-up from (s): the speed played before elsewhere)
    const JKL: [(&str, f64, f64, Option<f64>); 6] = [
        ("xm5-jkl L 2x", 2.0, 0.0, None),
        ("xm5-jkl L 4x", 4.0, 0.0, None),
        ("xm5-jkl L 4x warm", 4.0, 0.0, Some(45.0)),
        ("xm5-jkl J 1x", -1.0, 50.0, None),
        ("xm5-jkl J 2x", -2.0, 50.0, None),
        ("xm5-jkl J 2x warm", -2.0, 50.0, Some(15.0)),
    ];
    if o.wants("play-full")
        || o.wants("play-half-draft")
        || o.wants("play-flog2")
        || o.wants("play-dwg")
        || o.wants("play-graded")
        || o.wants("play-graded-nr")
        || JKL.iter().any(|(name, ..)| o.wants(name))
    {
        let mut s = Session::default();
        if let Some(a) = import(&mut s, PLAY) {
            let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 60.0)], true);
            for _ in 0..o.repeat {
                if o.wants("play-full") {
                    play(&s, seq, o, "xm5-play-full", play_s, 1.0, Tick::ZERO, &mut rows);
                }
                if o.wants("play-half-draft") {
                    // Program monitor at 1/2 with Settings ▸ Playback ▸ Draft decoding (the GPU
                    // scales hardware frames: no more CPU work than at Full)
                    let fps = s.project.sequence(seq).map(|q| q.settings.frame_rate.as_f64()).unwrap_or(29.97);
                    let mut bench = new_bench(&s, play_s);
                    bench.draft = true;
                    let mut display = Display::new(o.gpu);
                    let smp = Sampler::start();
                    let r = bench.play(&s, seq, &mut display, 0.5, Tick::ZERO, "xm5-play-half-draft");
                    rows.push(play_row("xm5-play-half-draft", &r, fps, &smp.finish(), &display.adapter));
                }
                let secs = if o.quick { 4.0 } else { 10.0 };
                for (name, speed, from, warm) in JKL {
                    if o.wants(name) {
                        play_after(&s, seq, o, name, secs, speed, Tick::from_seconds_f64(from), warm.map(Tick::from_seconds_f64), &mut rows);
                    }
                }
            }
            if o.wants("play-flog2") {
                interpret_flog2(&mut s, seq);
                for _ in 0..o.repeat {
                    play(&s, seq, o, "xm5-play-flog2", play_s, 1.0, Tick::ZERO, &mut rows);
                }
            }
            if o.wants("play-dwg") {
                interpret_flog2(&mut s, seq);
                set_dwg(&mut s, seq);
                for _ in 0..o.repeat {
                    play(&s, seq, o, "xm5-play-dwg", play_s, 1.0, Tick::ZERO, &mut rows);
                }
            }
            if o.wants("play-graded") {
                grade(&mut s, seq);
                for _ in 0..o.repeat {
                    play(&s, seq, o, "xm5-play-graded", play_s, 1.0, Tick::ZERO, &mut rows);
                }
            }
            // (fork G2.4: the graded case plus a grade node with spatial noise reduction)
            if o.wants("play-graded-nr") {
                if !o.wants("play-graded") {
                    grade(&mut s, seq);
                }
                add_nr_node(&mut s, seq);
                for _ in 0..o.repeat {
                    play(&s, seq, o, "xm5-play-graded-nr", play_s, 1.0, Tick::ZERO, &mut rows);
                }
            }
        } else {
            eprintln!("xm5 play: skipped (clip unavailable)");
        }
    }
    if o.wants("scrub") {
        rows.extend(scrub(o));
    }
    if o.wants("multicut") {
        let mut s = Session::default();
        let ids: Option<Vec<ItemId>> = [PLAY, P2398, CLIP_B, LONG].iter().map(|n| import(&mut s, n)).collect();
        if let Some(ids) = ids {
            // 20 cuts of 3 s, alternating A B C D, each from a different place in its clip
            let cuts: Vec<(ItemId, f64, f64)> = (0..20).map(|i| (ids[i % 4], 1.0 + (i / 4) as f64 * 5.3, 3.0)).collect();
            let seq = cuts_sequence(&mut s, 3840, 2160, &cuts, false);
            let mut at = Tick::ZERO;
            let starts: Vec<i64> = cuts
                .iter()
                .map(|c| {
                    let f = FPS_29_97.frame_at(at);
                    at += FPS_29_97.snap(Tick::from_seconds_f64(c.2));
                    f
                })
                .collect();
            for _ in 0..o.repeat {
                play_cuts(&s, seq, o, "xm5-multicut", if o.quick { 12.0 } else { 60.0 }, &starts, &mut rows);
            }
        }
    }
    // (fork 6.5) realistic cut spacing: a cut every 2 / 3 / 4 / 5 s, A B C D in turn, each clip
    // entered mid-GOP (0.45 s past a second; keyint is 1 s) at a different place every lap.
    // `--only cutspacing` runs all four, `--only "cutspacing 3s"` one.
    for spacing in [2.0f64, 3.0, 4.0, 5.0] {
        let name = format!("xm5-cutspacing {spacing:.0}s");
        if !o.wants(&name) {
            continue;
        }
        let mut s = Session::default();
        let ids: Option<Vec<ItemId>> = [PLAY, P2398, CLIP_B, LONG].iter().map(|n| import(&mut s, n)).collect();
        let Some(ids) = ids else {
            eprintln!("xm5 {name}: skipped (clips unavailable)");
            continue;
        };
        let secs = if o.quick { 12.0 } else { 60.0 };
        let n = (secs / spacing).ceil() as usize;
        let cuts: Vec<(ItemId, f64, f64)> = (0..n).filter_map(|i| Some((*ids.get(i % 4)?, 0.45 + ((i / 4) as f64 * 3.7) % 20.0, spacing))).collect();
        let rate = FPS_29_97;
        let mut at = Tick::ZERO;
        let mut starts = Vec::new();
        for _ in &cuts {
            starts.push(rate.frame_at(at));
            at += rate.snap(Tick::from_seconds_f64(spacing));
        }
        let seq = cuts_sequence(&mut s, 3840, 2160, &cuts, false);
        for _ in 0..o.repeat {
            play_cuts(&s, seq, o, &name, secs, &starts, &mut rows);
        }
    }
    if [
        "xm5-export-4k",
        "xm5-export-4k graded",
        "xm5-export-6k",
        "xm5-export-6k graded",
        "xm5-export-422 graded",
        "xm5-export-422 master graded",
        "xm5-export-422 prores graded",
    ]
    .iter()
    .any(|n| o.wants(n))
        || asks_tnr(o, "xm5-export-tnr")
    {
        rows.extend(export(o));
    }
    // only when asked for by name: hours long
    if o.only.as_deref().is_some_and(|x| x.contains("soak")) {
        rows.extend(soak(o));
    }
    // (fork 6.2) the same cases on automatic proxies (`--only proxies`)
    rows.extend(proxy_cases(o));
    // (fork 6.3) the render cache: only when asked for by name (they render the cache first)
    rows.extend(cache_cases(o));
    if o.wants("422-play") {
        let mut s = Session::default();
        if let Some(a) = import(&mut s, C422) {
            let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 20.0)], true);
            for _ in 0..o.repeat {
                play(&s, seq, o, "xm5-422-play", if o.quick { 8.0 } else { 19.0 }, 1.0, Tick::ZERO, &mut rows);
            }
        }
    }
    rows
}

// ------------------------------------------------------------------------------- render cache

/// Whether `--only` names a render-cache case (they are slow: they render the cache first).
fn asks_cache(o: &Opts, name: &str) -> bool {
    o.only.as_deref().is_some_and(|x| (x.contains("cached") || x.contains("cache-while")) && name.contains(x))
}

/// Fill the render cache of the active sequence (the app's Smart mode) and wait for it, at most
/// `budget`. Returns the cache status.
fn fill_cache(s: &mut Session, budget: Duration) -> Value {
    use filmcraft_engine::render_cache;
    let t0 = Instant::now();
    loop {
        render_cache::tick(s, true);
        if s.previews.cache.queued().is_empty() && s.background.is_idle() {
            break;
        }
        if t0.elapsed() > budget {
            eprintln!("xm5 cache: not finished after {:.0} s; playing what is cached", budget.as_secs_f64());
            break;
        }
        let _ = s.background.wait_idle(Duration::from_secs(5));
    }
    let st = render_cache::status_json(s);
    eprintln!(
        "xm5 cache: {} parts, {:.2} GB in {:.0} s ({} failed{})",
        st["parts"],
        st["bytes"].as_f64().unwrap_or(0.0) / 1e9,
        t0.elapsed().as_secs_f64(),
        st["failed"],
        st["lastError"].as_str().map(|e| format!(", last: {e}")).unwrap_or_default()
    );
    st
}

/// Delete the session's cache files (the bench leaves no gigabytes behind).
fn drop_cache(s: &Session) {
    s.background.cancel_kind(filmcraft_engine::background::JobKind::Cache);
    let _ = s.background.wait_idle(Duration::from_secs(30));
    s.previews.delete(None);
    if let Some(d) = s.previews.dir() {
        let _ = std::fs::remove_dir_all(d);
    }
}

fn note_cache(rows: &mut [Value], st: &Value, fill_s: f64) {
    if let Some(r) = rows.last_mut() {
        r["cache_parts"] = st["parts"].clone();
        r["cache_bytes"] = st["bytes"].clone();
        r["cache_fill_s"] = json!(fill_s);
    }
}

/// `xm5-multicut cached`, `xm5-jkl L 2x cached` (play once the render cache is complete) and
/// `xm5-cache-while-playing` (play-full while the cache renders: the worker must wait for the
/// playback, as the UI hook makes it, so no frame drops: the GPU-stall guard).
fn cache_cases(o: &Opts) -> Vec<Value> {
    let mut rows = Vec::new();
    let fill_budget = Duration::from_secs(if o.quick { 600 } else { 1800 });
    if asks_cache(o, "xm5-multicut cached") {
        let mut s = Session::default();
        let ids: Option<Vec<ItemId>> = [PLAY, P2398, CLIP_B, LONG].iter().map(|n| import(&mut s, n)).collect();
        if let Some(ids) = ids {
            let cuts: Vec<(ItemId, f64, f64)> = (0..20).filter_map(|i| Some((*ids.get(i % 4)?, 1.0 + (i / 4) as f64 * 5.3, 3.0))).collect();
            let seq = cuts_sequence(&mut s, 3840, 2160, &cuts, false);
            let mut at = Tick::ZERO;
            let starts: Vec<i64> = cuts
                .iter()
                .map(|c| {
                    let f = FPS_29_97.frame_at(at);
                    at += FPS_29_97.snap(Tick::from_seconds_f64(c.2));
                    f
                })
                .collect();
            let t0 = Instant::now();
            let st = fill_cache(&mut s, fill_budget);
            let fill_s = t0.elapsed().as_secs_f64();
            for _ in 0..o.repeat {
                play_cuts(&s, seq, o, "xm5-multicut cached", if o.quick { 12.0 } else { 60.0 }, &starts, &mut rows);
                note_cache(&mut rows, &st, fill_s);
            }
            drop_cache(&s);
        } else {
            eprintln!("xm5 multicut cached: skipped (clips unavailable)");
        }
    }
    if asks_cache(o, "xm5-jkl L 2x cached") {
        let mut s = Session::default();
        if let Some(a) = import(&mut s, PLAY) {
            let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 60.0)], true);
            let t0 = Instant::now();
            let st = fill_cache(&mut s, fill_budget);
            let fill_s = t0.elapsed().as_secs_f64();
            let secs = if o.quick { 4.0 } else { 10.0 };
            for _ in 0..o.repeat {
                play_after(&s, seq, o, "xm5-jkl L 2x cached", secs, 2.0, Tick::ZERO, None, &mut rows);
                note_cache(&mut rows, &st, fill_s);
            }
            drop_cache(&s);
        } else {
            eprintln!("xm5 jkl L 2x cached: skipped (clip unavailable)");
        }
    }
    // (fork 6.7) fill the render cache of a clip with temporal noise reduction (every segment
    // needs rendering) and play it back: the cache's fill time is the cost of TNR
    if asks_tnr(o, "xm5-cache-tnr") {
        let mut s = Session::default();
        if let Some(a) = import(&mut s, PLAY) {
            let secs = if o.quick { 6.0 } else { 20.0 };
            let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, secs)], true);
            add_tnr_clip(&mut s, seq);
            let t0 = Instant::now();
            let st = fill_cache(&mut s, fill_budget);
            let fill_s = t0.elapsed().as_secs_f64();
            for _ in 0..o.repeat {
                play(&s, seq, o, "xm5-cache-tnr", secs.min(if o.quick { 4.0 } else { 15.0 }), 1.0, Tick::ZERO, &mut rows);
                note_cache(&mut rows, &st, fill_s);
            }
            drop_cache(&s);
        } else {
            eprintln!("xm5 cache-tnr: skipped (clip unavailable)");
        }
    }
    if asks_cache(o, "xm5-cache-while-playing") {
        use filmcraft_engine::background::Activity;
        let mut s = Session::default();
        if let Some(a) = import(&mut s, PLAY) {
            let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 60.0)], true);
            let play_s = if o.quick { 8.0 } else { 60.0 };
            for _ in 0..o.repeat {
                // the cache is building a part (the burst of decoding + encoding that stalled
                // compositing) when Play is pressed
                filmcraft_engine::render_cache::tick(&mut s, true);
                let t0 = Instant::now();
                while s.previews.cache.building().is_none() && t0.elapsed() < Duration::from_secs(20) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let building = s.previews.cache.building().is_some();
                // what the UI hook reports while the Program monitor plays
                s.background.set_activity(Activity { playing: true, ..Default::default() });
                play(&s, seq, o, "xm5-cache-while-playing", play_s, 1.0, Tick::ZERO, &mut rows);
                s.background.set_activity(Activity::default());
                let st = filmcraft_engine::render_cache::status_json(&s);
                if let Some(r) = rows.last_mut() {
                    r["cache_building_at_play"] = json!(building);
                    r["cache_parts"] = st["parts"].clone();
                }
            }
            drop_cache(&s);
        } else {
            eprintln!("xm5 cache-while-playing: skipped (clip unavailable)");
        }
    }
    rows
}

// ------------------------------------------------------------------------------------ ceilings

/// Frames in a clip according to ffprobe-free arithmetic on FilmCraft's own media info.
fn clip_frames(path: &Path) -> Option<i64> {
    let mut s = Session::default();
    let id = playback::import(&mut s, path);
    match s.project.item(id).map(|i| &i.kind) {
        Some(filmcraft_project::ItemKind::Media(m)) => {
            let v = m.info.video.as_ref()?;
            Some(v.frame_rate.frame_at(m.info.duration))
        }
        _ => None,
    }
}

/// Run ffmpeg and return (frames/s, wall s).
fn timed(ff: &Path, args: &[&str], frames: i64) -> Option<(f64, f64)> {
    let t0 = Instant::now();
    let out = Command::new(ff).args(["-hide_banner", "-v", "error", "-nostdin"]).args(args).output().ok()?;
    let dt = t0.elapsed().as_secs_f64();
    if !out.status.success() {
        eprintln!("ffmpeg {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or(""));
        return None;
    }
    Some((frames as f64 / dt, dt))
}

/// The hardware ceilings of FORK-PLAN §4, measured with the system ffmpeg (an oracle only).
fn ceilings(o: &Opts) -> Vec<Value> {
    let mut rows = Vec::new();
    let Some(ff) = crate::fixtures::ffmpeg() else {
        eprintln!("ceilings: no ffmpeg");
        return rows;
    };
    let dev = filmcraft_platform::ffmpeg::vaapi_device().unwrap_or("/dev/dri/renderD128").to_string();
    let (Some(clip420), Some(clip422)) = (clip(PLAY), clip(C422)) else {
        return rows;
    };
    let n420 = clip_frames(&clip420).unwrap_or(0);
    let n422 = clip_frames(&clip422).unwrap_or(0);
    let lim = if o.quick { "300" } else { "100000" };
    let n420 = if o.quick { n420.min(300) } else { n420 };
    let c420 = clip420.to_string_lossy().to_string();
    let c422 = clip422.to_string_lossy().to_string();
    let mut push = |what: &str, cmd: String, r: Option<(f64, f64)>| {
        if let Some((fps, wall)) = r {
            eprintln!("ceiling {what}: {fps:.1} fps");
            rows.push(json!({"case": format!("ceiling {what}"), "fps": fps, "wall_s": wall, "command": cmd, "load": load_avg()}));
        }
    };
    let hw = ["-hwaccel", "vaapi", "-hwaccel_device", &dev, "-hwaccel_output_format", "vaapi"];
    // decode, frames stay on the GPU
    let a: Vec<&str> = hw.iter().copied().chain(["-i", &c420, "-frames:v", lim, "-an", "-f", "null", "-"]).collect();
    push("decode 6.2K 4:2:0 (frames stay on GPU)", format!("ffmpeg {}", a.join(" ")), timed(&ff, &a, n420));
    // decode + download
    let a: Vec<&str> = hw.iter().copied().chain(["-i", &c420, "-frames:v", lim, "-an", "-vf", "hwdownload,format=p010", "-f", "null", "-"]).collect();
    push("decode 6.2K 4:2:0 + download P010", format!("ffmpeg {}", a.join(" ")), timed(&ff, &a, n420));
    // encode from frames already on the GPU (decode → scale_vaapi → hevc_vaapi), 4K and 6.2K
    for (label, vf) in
        [("encode 3840x2160 Main10 (GPU source)", "scale_vaapi=w=3840:h=2160:format=p010"), ("encode 6240x4160 Main10 (GPU source)", "scale_vaapi=format=p010")]
    {
        let a: Vec<&str> = hw
            .iter()
            .copied()
            .chain(["-i", &c420, "-frames:v", lim, "-an", "-vf", vf, "-c:v", "hevc_vaapi", "-profile:v", "main10", "-b:v", "100M", "-f", "null", "-"])
            .collect();
        push(label, format!("ffmpeg {}", a.join(" ")), timed(&ff, &a, n420));
    }
    // 4:2:2 software decode (the GPU can't)
    let a = ["-threads", "0", "-i", &c422, "-an", "-f", "null", "-"];
    push("CPU software decode 6.2K 4:2:2", format!("ffmpeg {}", a.join(" ")), timed(&ff, &a, n422));
    let a = ["-threads", "0", "-i", &c420, "-frames:v", lim, "-an", "-f", "null", "-"];
    push("CPU software decode 6.2K 4:2:0", format!("ffmpeg {}", a.join(" ")), timed(&ff, &a, n420));
    rows
}

// ------------------------------------------------------------------------------------ decode

/// Sequential decode of the 6.2K clip through FilmCraft's media stack (container source, GOP
/// cache, registered decoder, conversion), as playback reads it.
fn decode(o: &Opts) -> Vec<Value> {
    let mut rows = Vec::new();
    for name in [PLAY, C422] {
        let Some(path) = clip(name) else { continue };
        let mut s = Session::default();
        let id = playback::import(&mut s, &path);
        let Some(src) = s.media.source_for(&s.project, id, s.services.as_ref()) else {
            eprintln!("xm5 decode {name}: no source");
            continue;
        };
        let info = src.info();
        let rate = info.frame_rate();
        let n = rate.frame_at(info.duration).max(1);
        let n = if o.quick { n.min(120) } else { n.min(600) };
        let hw0 = filmcraft_codecs::hw::hw_stats().frames;
        let smp = Sampler::start();
        let (t0, c0) = (Instant::now(), filmcraft_ui_egui::frames::process_cpu_time().unwrap_or_default());
        let mut done = 0;
        for f in 0..n {
            if let Err(e) = src.video_frame(filmcraft_media::FrameRequest::full(rate.tick_of(f))) {
                eprintln!("xm5 decode {name} frame {f}: {e}");
                break;
            }
            done += 1;
        }
        let dt = t0.elapsed().as_secs_f64();
        let cpu = ms(filmcraft_ui_egui::frames::process_cpu_time().unwrap_or_default().saturating_sub(c0));
        let smp = smp.finish();
        let fps = done as f64 / dt;
        eprintln!("xm5 decode {name}: {fps:.1} fps, CPU {:.1} ms/frame", cpu / done.max(1) as f64);
        rows.push(json!({
            "case": format!("decode {name}"), "frames": done, "fps": fps, "cpu_ms_per_frame": cpu / done.max(1) as f64,
            "hw_frames": filmcraft_codecs::hw::hw_stats().frames - hw0, "sampled": smp.json(), "load": load_avg(),
        }));
    }
    rows
}

// ------------------------------------------------------------------------------------ scrub

/// 50 random seeks across the 5-minute clip: time until the exact full-resolution frame is on
/// screen.
fn scrub(o: &Opts) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut s = Session::default();
    let Some(a) = import(&mut s, LONG) else {
        eprintln!("xm5 scrub: skipped (clip unavailable)");
        return rows;
    };
    let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 300.0)], false);
    let seeks = if o.quick { 10 } else { 50 };
    for _ in 0..o.repeat {
        let bench = new_bench(&s, 0.0);
        let mut display = Display::new(o.gpu);
        let smp = Sampler::start();
        let r = bench.scrub(&s, seq, &mut display, 1.0, seeks, 0, 0, Duration::from_secs(20));
        let smp = smp.finish();
        eprintln!("xm5 scrub: p50 {:.0} ms, p95 {:.0} ms, {} timeouts", pct(&r.seek_ms, 0.5), pct(&r.seek_ms, 0.95), r.timeouts);
        rows.push(json!({
            "case": "xm5-scrub", "seeks": seeks, "seek_p50_ms": pct(&r.seek_ms, 0.5), "seek_p95_ms": pct(&r.seek_ms, 0.95),
            "seek_max_ms": pct(&r.seek_ms, 1.0), "timeouts": r.timeouts, "decoded_per_seek": r.gop.decoded as f64 / seeks as f64,
            "process_cpu_ms": r.process_cpu_ms, "adapter": display.adapter, "sampled": smp.json(), "load": r.load,
        }));
    }
    rows
}

// ------------------------------------------------------------------------------------ export

/// 60 s of the 6.2K clip exported to HEVC Main10: 3840×2160 and 6240×4160, ungraded and graded.
fn export(o: &Opts) -> Vec<Value> {
    let mut rows = Vec::new();
    let secs: f64 = if o.quick { 5.0 } else { 60.0 };
    let dir = std::env::temp_dir().join(format!("filmcraft-bench-xm5-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return rows;
    }
    // (the last: 4K with temporal noise reduction on the clip, fork 6.7; only when named)
    // (source, chroma of the output file)
    let cases: [(&str, u32, u32, bool, &str, &str); 8] = [
        ("xm5-export-4k", 3840, 2160, false, PLAY, "420"),
        ("xm5-export-4k graded", 3840, 2160, true, PLAY, "420"),
        ("xm5-export-6k", 6240, 4160, false, PLAY, "420"),
        // fork U0.2.3b (FORK-PLAN §1a.58): the graded 6.2K export, reported (aim ≥ 24 fps)
        ("xm5-export-6k graded", 6240, 4160, true, PLAY, "420"),
        ("xm5-export-tnr", 3840, 2160, false, PLAY, "420"),
        // fork X422 (FORK-PLAN §1a.68): the 6.2K 4:2:2 original graded, (a) to a 4:2:0 file
        // (aim ≥ 24 fps) and (b) to the 4:2:2 HEVC master; the clip is 20 s long
        ("xm5-export-422 graded", 6240, 4160, true, C422, "420"),
        ("xm5-export-422 master graded", 6240, 4160, true, C422, "422"),
        // (b)'s comparison: our CPU ProRes 422 HQ encoder ("prores" here: the format, not a chroma)
        ("xm5-export-422 prores graded", 6240, 4160, true, C422, "prores"),
    ];
    for (name, w, h, graded, src, chroma) in cases {
        let tnr = name.contains("tnr");
        let wanted = if tnr { asks_tnr(o, name) } else { o.wants(name) };
        if !wanted {
            continue;
        }
        let mut s = Session::default();
        let Some(a) = import(&mut s, src) else { continue };
        let secs = if src == C422 { secs.min(19.0) } else { secs };
        let seq = cuts_sequence(&mut s, w, h, &[(a, 0.0, secs)], true);
        if graded {
            grade(&mut s, seq);
        }
        if tnr {
            add_tnr_clip(&mut s, seq);
        }
        let frames = s.project.sequence(seq).map(|q| q.settings.frame_rate.frame_at(q.duration())).unwrap_or(0);
        // FILMCRAFT_BENCH_ENGINES=single|dual|auto: Export ▸ Export engines (fork 6E.3; default auto)
        let engines = std::env::var("FILMCRAFT_BENCH_ENGINES").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "auto".into());
        for _ in 0..o.repeat {
            let out = dir.join("out.mp4");
            let smp = Sampler::start();
            let g0 = filmcraft_codecs::gop_stats();
            let st0 = filmcraft_export::stage_times();
            let _ = filmcraft_export::trace::take();
            let (t0, c0) = (Instant::now(), filmcraft_ui_egui::frames::process_cpu_time().unwrap_or_default());
            let mut params = json!({"path": out.to_string_lossy(), "format": "hevc", "bitDepth": "10", "bitrateKbps": 100_000, "bitrateMode": "vbr1Pass", "exportEngines": engines, "chroma": chroma, "wait": true});
            // FILMCRAFT_BENCH_ENC_SPEED=speed|balanced|quality: the hardware encoder's level (fork X422)
            if let Some(v) = std::env::var("FILMCRAFT_BENCH_ENC_SPEED").ok().filter(|v| !v.is_empty()) {
                params["hwEncodeSpeed"] = json!(v);
            }
            if chroma == "prores" {
                params = json!({"path": dir.join("out.mov").to_string_lossy(), "format": "prores", "proresProfile": "hq", "wait": true});
            }
            let r = s.execute("file.exportMedia", params);
            let dt = t0.elapsed().as_secs_f64();
            let cpu = ms(filmcraft_ui_egui::frames::process_cpu_time().unwrap_or_default().saturating_sub(c0));
            let smp = smp.finish();
            if let Err(e) = r {
                eprintln!("{name}: {e}");
                break;
            }
            if let Some(e) = s.jobs.last().and_then(|j| j.progress.error.lock().ok().and_then(|g| g.clone())) {
                eprintln!("{name}: {e}");
                break;
            }
            let fps = frames as f64 / dt;
            let g = filmcraft_codecs::gop_stats() - g0;
            eprintln!("{name}: {frames} frames in {dt:.1} s = {fps:.1} fps (GOP seeks {}, decoded {}, evicted {})", g.seeks, g.decoded, g.evicted);
            // FILMCRAFT_BENCH_KEEP=<dir>: keep the output (`<case>.mp4`) for comparisons
            if let Some(keep) = std::env::var_os("FILMCRAFT_BENCH_KEEP").filter(|k| !k.is_empty()) {
                let _ = std::fs::copy(&out, std::path::Path::new(&keep).join(format!("{}.mp4", name.replace(' ', "-"))));
            }
            rows.push(json!({
                "case": name, "frames": frames, "seconds": dt, "fps": fps, "realtime": fps / FPS_29_97.as_f64(),
                "cpu_ms_per_frame": cpu / frames.max(1) as f64, "mbytes": std::fs::metadata(if chroma == "prores" { dir.join("out.mov") } else { out.clone() }).map(|m| m.len()).unwrap_or(0) as f64 / 1e6,
                "sampled": smp.json(), "load": load_avg(), "gop_seeks": g.seeks, "gop_decoded": g.decoded, "gop_evicted": g.evicted,
                "engines": engines, "chroma": chroma, "lanes": filmcraft_export::lanes::last_lanes(), "backends": filmcraft_codecs::hw::backend_stats(),
                // wall seconds per export stage, summed over lanes and threads (fork 6E.3 S11)
                "stages_s": filmcraft_export::stage_times().iter().zip(&st0).map(|((k, b), (_, a))| (k.name().to_string(), json!(b.saturating_sub(*a) as f64 / 1e9))).collect::<serde_json::Map<_, _>>(),
                // FILMCRAFT_EXPORT_TRACE=1: render / encode breakdown in ms (lock waits, GPU record, VA encode)
                "trace": filmcraft_export::trace::on().then(filmcraft_export::trace::take),
            }));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    rows
}

// ------------------------------------------------------------------------------------ soak

/// Play the 6.2K sequence in a loop for `FILMCRAFT_SOAK_MINUTES` (default 120) and sample memory
/// every minute: RSS, GPU memory in use, the decode cache's GPU memory and live VA surfaces must
/// not grow (FORK-PLAN Phase 2 acceptance).
fn soak(o: &Opts) -> Vec<Value> {
    let minutes: u64 = std::env::var("FILMCRAFT_SOAK_MINUTES").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    let mut s = Session::default();
    let Some(a) = import(&mut s, PLAY) else { return Vec::new() };
    let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 60.0)], true);
    let bench = new_bench(&s, 55.0);
    let mut display = Display::new(o.gpu);
    let dev = amd_gpu_sysfs();
    let vram = || dev.as_ref().and_then(|d| read_num(&d.join("mem_info_vram_used"))).unwrap_or(0.0) / 1e6;
    let t0 = Instant::now();
    let mut samples = Vec::new();
    let (mut shown, mut dropped, mut plays) = (0u64, 0u64, 0u64);
    while t0.elapsed() < Duration::from_secs(minutes * 60) {
        let r = bench.play(&s, seq, &mut display, 1.0, Tick::ZERO, "soak");
        shown += r.shown;
        dropped += r.dropped;
        plays += 1;
        #[cfg(target_os = "linux")]
        let surfaces = filmcraft_platform::va::live_surfaces();
        #[cfg(not(target_os = "linux"))]
        let surfaces = 0;
        let row = json!({
            "minute": t0.elapsed().as_secs_f64() / 60.0, "rss_mb": rss_bytes().unwrap_or(0.0) / 1e6, "vram_mb": vram(),
            "cache_vram_mb": filmcraft_codecs::vram_usage().0 as f64 / 1e6, "va_surfaces": surfaces, "shown": r.shown, "dropped": r.dropped,
        });
        eprintln!("soak {row}");
        samples.push(row);
    }
    let first = samples.first().cloned().unwrap_or_default();
    let last = samples.last().cloned().unwrap_or_default();
    let grow = |k: &str| last[k].as_f64().unwrap_or(0.0) - first[k].as_f64().unwrap_or(0.0);
    vec![json!({
        "case": "xm5-soak", "minutes": minutes, "plays": plays, "shown": shown, "dropped": dropped,
        "rss_growth_mb": grow("rss_mb"), "vram_growth_mb": grow("vram_mb"), "cache_vram_growth_mb": grow("cache_vram_mb"),
        "va_surfaces_growth": grow("va_surfaces"), "samples": samples, "load": load_avg(),
    })]
}

/// (fork C1 acceptance) Edit and play comps A and B (`comp-1080`) for `FILMCRAFT_SOAK_MINUTES`
/// (default 30): each round changes parameters through `comp.*` commands, adds and deletes a node,
/// undoes and redoes, then plays 2 s; RSS and VRAM are sampled per round and must not grow.
fn comp_soak(o: &Opts) -> Vec<Value> {
    let minutes: u64 = std::env::var("FILMCRAFT_SOAK_MINUTES").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let Some((mut s, seq)) = playback::scenario_session("comp-1080") else { return Vec::new() };
    let comps: Vec<u64> = s
        .project
        .sequence(seq)
        .map(|q| {
            q.video_tracks
                .iter()
                .flat_map(|t| t.items.iter())
                .map(|i| i.item)
                .filter(|i| matches!(s.project.item(*i).map(|p| &p.kind), Some(filmcraft_project::ItemKind::Comp(_))))
                .map(|i| i.0)
                .collect()
        })
        .unwrap_or_default();
    let [a, b] = comps.as_slice() else {
        eprintln!("comp-soak: expected comps A and B, found {comps:?}");
        return Vec::new();
    };
    let (a, b) = (*a, *b);
    let bench = new_bench(&s, 2.0);
    let mut display = Display::new(o.gpu);
    let dev = amd_gpu_sysfs();
    let vram = || dev.as_ref().and_then(|d| read_num(&d.join("mem_info_vram_used"))).unwrap_or(0.0) / 1e6;
    let t0 = Instant::now();
    let mut samples = Vec::new();
    let (mut shown, mut dropped, mut rounds, mut errors) = (0u64, 0u64, 0u64, 0u64);
    while t0.elapsed() < Duration::from_secs(minutes * 60) {
        let k = rounds as f64;
        let edits = [
            ("comp.setParam", json!({"comp": a, "node": 3, "param": "threshold", "value": 10.0 + (k * 7.0) % 60.0})),
            ("comp.setParam", json!({"comp": a, "node": 5, "param": "opacity", "value": 50.0 + (k * 11.0) % 50.0})),
            ("comp.setParam", json!({"comp": b, "node": 3, "param": "scale", "value": 40.0 + (k * 13.0) % 80.0})),
            ("comp.addNode", json!({"comp": b, "kind": "blur"})),
            ("edit.undo", json!({})),
            ("edit.redo", json!({})),
            ("edit.undo", json!({})),
        ];
        for (id, p) in edits {
            if s.execute(id, p).is_err() {
                errors += 1;
            }
        }
        let r = bench.play(&s, seq, &mut display, 1.0, Tick::ZERO, "comp-soak");
        shown += r.shown;
        dropped += r.dropped;
        rounds += 1;
        if rounds % 10 == 1 {
            let row = json!({"minute": t0.elapsed().as_secs_f64() / 60.0, "rss_mb": rss_bytes().unwrap_or(0.0) / 1e6, "vram_mb": vram(), "shown": r.shown, "dropped": r.dropped});
            eprintln!("comp-soak {row}");
            samples.push(row);
        }
    }
    let first = samples.get(1).or(samples.first()).cloned().unwrap_or_default();
    let last = samples.last().cloned().unwrap_or_default();
    let grow = |k: &str| last[k].as_f64().unwrap_or(0.0) - first[k].as_f64().unwrap_or(0.0);
    vec![json!({
        "case": "comp-soak", "minutes": minutes, "rounds": rounds, "errors": errors, "shown": shown, "dropped": dropped,
        "rss_growth_mb": grow("rss_mb"), "vram_growth_mb": grow("vram_mb"), "samples": samples, "load": load_avg(),
    })]
}

// ------------------------------------------------------------------------------------ proxies

/// (fork 6.2) The playback cases on automatic proxies: `xm5-multicut proxies`, `xm5-422-play
/// proxies`, `xm5-jkl L 2x proxies`. Only when asked for by a name with "proxies" in it (`--only
/// proxies` runs all three). Each imports its clips (which queues their proxies, or attaches the
/// ones a previous run left in the clips' `Proxies` folder), waits for the background worker to
/// finish them (a `… proxies create` row says how long), then plays with Proxies ▸ On (the bench's
/// frame server gives no paused / playing hint and nothing reports playback to the worker, so
/// Auto would read the originals).
fn proxy_cases(o: &Opts) -> Vec<Value> {
    let mut rows = Vec::new();
    let asked = |name: &str| o.only.as_deref().is_some_and(|x| x.contains("proxies")) && o.wants(name);
    if asked("xm5-multicut proxies") {
        let mut s = Session::default();
        s.auto_proxies.enabled = true;
        let ids: Option<Vec<ItemId>> = [PLAY, P2398, CLIP_B, LONG].iter().map(|n| import(&mut s, n)).collect();
        match ids {
            Some(ids) if wait_for_proxies(&mut s, "xm5-multicut proxies", &mut rows) => {
                // the cuts of `xm5-multicut`: 20 cuts of 3 s, A B C D in turn
                let cuts: Vec<(ItemId, f64, f64)> = (0..20).filter_map(|i| Some((*ids.get(i % 4)?, 1.0 + (i / 4) as f64 * 5.3, 3.0))).collect();
                let seq = cuts_sequence(&mut s, 3840, 2160, &cuts, false);
                let mut at = Tick::ZERO;
                let starts: Vec<i64> = cuts
                    .iter()
                    .map(|c| {
                        let f = FPS_29_97.frame_at(at);
                        at += FPS_29_97.snap(Tick::from_seconds_f64(c.2));
                        f
                    })
                    .collect();
                for _ in 0..o.repeat {
                    play_cuts(&s, seq, o, "xm5-multicut proxies", if o.quick { 12.0 } else { 60.0 }, &starts, &mut rows);
                }
            }
            Some(_) => {}
            None => eprintln!("xm5-multicut proxies: skipped (clips unavailable)"),
        }
    }
    if asked("xm5-422-play proxies") {
        let mut s = Session::default();
        s.auto_proxies.enabled = true;
        match import(&mut s, C422) {
            Some(a) if wait_for_proxies(&mut s, "xm5-422-play proxies", &mut rows) => {
                let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 20.0)], true);
                for _ in 0..o.repeat {
                    play(&s, seq, o, "xm5-422-play proxies", if o.quick { 8.0 } else { 19.0 }, 1.0, Tick::ZERO, &mut rows);
                }
            }
            Some(_) => {}
            None => eprintln!("xm5-422-play proxies: skipped (clip unavailable)"),
        }
    }
    if asked("xm5-jkl L 2x proxies") {
        let mut s = Session::default();
        s.auto_proxies.enabled = true;
        match import(&mut s, PLAY) {
            Some(a) if wait_for_proxies(&mut s, "xm5-jkl L 2x proxies", &mut rows) => {
                let seq = cuts_sequence(&mut s, 3840, 2160, &[(a, 0.0, 60.0)], true);
                for _ in 0..o.repeat {
                    play_after(&s, seq, o, "xm5-jkl L 2x proxies", if o.quick { 4.0 } else { 10.0 }, 2.0, Tick::ZERO, None, &mut rows);
                }
            }
            Some(_) => {}
            None => eprintln!("xm5-jkl L 2x proxies: skipped (clip unavailable)"),
        }
    }
    rows
}

/// Wait (at most an hour) for the session's automatic proxies, then switch Proxies to On. Adds a
/// `<case> create` row (seconds, proxies ready / failed, their files). False when none is usable.
fn wait_for_proxies(s: &mut Session, case: &str, rows: &mut Vec<Value>) -> bool {
    let t0 = Instant::now();
    let mut last_note = Instant::now();
    loop {
        s.poll_persistence();
        if s.auto_proxies.pending() == 0 {
            break;
        }
        if t0.elapsed() > Duration::from_secs(3600) {
            eprintln!("{case}: proxies not done after an hour; cancelling");
            let _ = s.execute("media.cancelProxies", json!({}));
            break;
        }
        if last_note.elapsed() > Duration::from_secs(10) {
            let st = s.execute("media.proxyStatus", json!({})).unwrap_or_default();
            eprintln!("{case}: making proxies, {:.0} % ({} queued, {} building)", st["progress"].as_f64().unwrap_or(0.0) * 100.0, st["queued"], st["building"]);
            last_note = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let st = s.execute("media.proxyStatus", json!({})).unwrap_or_default();
    let files: Vec<String> = s
        .project
        .items
        .values()
        .filter_map(|i| match &i.as_media()?.proxy {
            Some(filmcraft_project::MediaRef::File { path }) => Some(path.clone()),
            _ => None,
        })
        .collect();
    let secs = t0.elapsed().as_secs_f64();
    eprintln!("{case}: proxies ready in {secs:.1} s ({} ready, {} failed): {files:?}", st["ready"], st["failed"]);
    rows.push(json!({"case": format!("{case} create"), "seconds": secs, "ready": st["ready"], "failed": st["failed"], "hevc_encode": st["hevcEncode"], "proxies": files, "status": st["items"]}));
    if st["ready"].as_u64().unwrap_or(0) == 0 {
        eprintln!("{case}: skipped (no proxy could be made)");
        return false;
    }
    if let Err(e) = s.execute("media.proxyMode", json!({"mode": "on"})) {
        eprintln!("{case}: {e}");
        return false;
    }
    true
}
