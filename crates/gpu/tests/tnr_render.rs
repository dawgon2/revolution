//! Temporal noise reduction through the whole GPU render (fork 6.7 integration): a clip with
//! clip temporal NR (`TrackItem::temporal_nr`, fork 6.7b) and a grade, rendered by the export renderer with temporal access (the
//! export / render-cache / paused-frame path: `plan_frame` attaches the neighbouring source frames
//! and their flows, the GPU blends them), matches the CPU reference, and without temporal access
//! the GPU leaves it out.

use std::sync::Arc;

use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaInfo, MediaKind, MediaSource, SharedSource, VideoStreamInfo};
use filmcraft_project::grade::{Grade, GradeNode};
use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Param, ParamValue, Project, SequenceSettings, TrackKind};
use filmcraft_render::tnr::fixture;
use filmcraft_render::{Image, RenderOptions, SourceMap};
use filmcraft_time::{FrameRate, Tick, TimeRange};

const FPS: FrameRate = FrameRate::FPS_24;

struct Fixture {
    info: MediaInfo,
    frames: Vec<Arc<VideoFrame>>,
}

impl MediaSource for Fixture {
    fn info(&self) -> &MediaInfo {
        &self.info
    }
    fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let i = FPS.frame_at(req.time).clamp(0, fixture::FRAMES as i64 - 1) as usize;
        self.frames.get(i).cloned().ok_or(filmcraft_media::MediaError::NoStream("video"))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn project(frames: f64) -> (Project, filmcraft_project::ItemId, SourceMap) {
    let info = MediaInfo {
        name: format!("tnr-gpu-{frames}"),
        kind: MediaKind::Movie,
        duration: FPS.tick_of(fixture::FRAMES as i64),
        video: Some(VideoStreamInfo {
            camera_color_space: None,
            width: fixture::W as u32,
            height: fixture::H as u32,
            frame_rate: FPS,
            par: (1, 1),
            codec: "test".into(),
            pixel_format: "RGBA float".into(),
            color: filmcraft_color::ColorInfo::SRGB_FULL,
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: Vec::new(),
        container: "test".into(),
        start_timecode: None,
        file_size: None,
    };
    // (half floats on the GPU: the source draws are near-lossless, as in lane A's parity test)
    let frames_v = fixture::frames(fixture::NOISE, 11).into_iter().map(|im| Arc::new(VideoFrame::rgba_f32(im.w as u32, im.h as u32, im.px))).collect();
    let mut p = Project::new("tnr-gpu");
    let clip = MediaClip {
        media: MediaRef::File { path: "tnr.mov".into() },
        info: info.clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("tnr", Label::Iris, ItemKind::Media(clip), None);
    let size = (fixture::W as u32, fixture::H as u32);
    let seq = p.new_sequence("s", SequenceSettings { width: size.0, height: size.1, frame_rate: FPS, ..Default::default() }, 1, 0, None);
    let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(fixture::FRAMES as i64)), FPS).unwrap();
    for e in &mut ti.effects {
        filmcraft_project::resolve_auto_points(e, size, size);
    }
    let mut n = GradeNode::new(1);
    n.params.insert("saturation".into(), Param::new(ParamValue::Float(1.2)));
    ti.grade = Some(Grade { nodes: vec![n], bypass: false, ..Default::default() });
    let mut tnr = filmcraft_project::TemporalNr::default();
    for (k, v) in [("luma", 1.0), ("chroma", 0.8), ("frames", frames)] {
        tnr.params.insert(k.into(), Param::new(ParamValue::Float(v)));
    }
    ti.temporal_nr = Some(tnr);
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let mut map = SourceMap::default();
    map.0.insert(item, Arc::new(Fixture { info, frames: frames_v }) as SharedSource);
    (p, seq, map)
}

/// (mean, max) absolute difference over the colour channels.
fn diff(a: &Image, b: &Image) -> (f32, f32) {
    assert_eq!((a.w, a.h), (b.w, b.h));
    let (mut s, mut m, mut n) = (0.0f64, 0.0f32, 0usize);
    for (p, q) in a.px.as_chunks::<4>().0.iter().zip(b.px.as_chunks::<4>().0) {
        for k in 0..3 {
            let d = (p[k] - q[k]).abs();
            s += f64::from(d);
            m = m.max(d);
            n += 1;
        }
    }
    ((s / n.max(1) as f64) as f32, m)
}

#[test]
fn gpu_temporal_render_matches_the_cpu_reference() {
    let Some(mut gpu) = filmcraft_gpu::ExportRenderer::new() else {
        eprintln!("SKIPPED (tnr render): no adapter");
        return;
    };
    let t = FPS.tick_of(fixture::MID as i64);
    for frames in [1.0, 2.0] {
        let (p, seq, map) = project(frames);
        let temporal = RenderOptions { temporal: true, ..Default::default() };
        let t0 = std::time::Instant::now();
        let cpu = filmcraft_render::render_sequence(&p, seq, t, temporal, &map);
        let cpu_ms = t0.elapsed().as_secs_f64() * 1e3;
        let t1 = std::time::Instant::now();
        let g = gpu.render(&p, seq, t, temporal, &map).expect("a GPU frame");
        eprintln!(
            "tnr render ±{frames} at {}×{} (synthetic, flows cached by the CPU render): CPU {cpu_ms:.0} ms, GPU path {:.0} ms",
            fixture::W,
            fixture::H,
            t1.elapsed().as_secs_f64() * 1e3
        );
        let (mean, max) = diff(&g, &cpu);
        assert!(mean < 2e-3 && max < 0.03, "±{frames}: GPU vs CPU mean {mean}, max {max}");
        // without temporal access the GPU leaves the op out: a different picture
        let live = gpu.render(&p, seq, t, RenderOptions::default(), &map).expect("a GPU frame");
        let (moved, _) = diff(&live, &g);
        assert!(moved > 2.0 * mean.max(1e-4), "±{frames}: temporal NR changed the GPU picture ({moved} vs parity {mean})");
    }
}

/// A clip whose grade has only temporal NR keeps the plain draw on the GPU without temporal
/// access (playback at a reduced monitor scale): the same pixels as the clip with no grade.
#[test]
fn gpu_temporal_only_grade_keeps_the_plain_path_without_temporal_access() {
    let Some(mut gpu) = filmcraft_gpu::ExportRenderer::new() else {
        eprintln!("SKIPPED (tnr plain path): no adapter");
        return;
    };
    let (mut p, seq, map) = project(1.0);
    let t = FPS.tick_of(fixture::MID as i64);
    let mut it = p.sequence(seq).map(|q| q.video_tracks[0].items[0].clone()).expect("the clip");
    if let Some(g) = it.grade.as_mut() {
        g.nodes[0].params.remove("saturation");
    }
    let half = RenderOptions { scale: 0.5, ..Default::default() };
    p.sequence_mut(seq).expect("seq").video_tracks[0].items[0] = it.clone();
    let tnr_only = gpu.render(&p, seq, t, half, &map).expect("a GPU frame");
    it.grade = None;
    p.sequence_mut(seq).expect("seq").video_tracks[0].items[0] = it;
    let plain = gpu.render(&p, seq, t, half, &map).expect("a GPU frame");
    assert_eq!(tnr_only.px, plain.px, "temporal-only grade without temporal access: the plain picture");
}
