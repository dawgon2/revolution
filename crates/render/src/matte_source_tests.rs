//! Magic Mask mattes reaching the render (fork G3.2): a registered [`MatteSource`] limits its
//! node in `render_sequence` output; without a matte (dead or unknown token, nothing tracked)
//! the node changes nothing.

use std::sync::Arc;

use super::*;
use crate::matte::{BitmapMatte, MatteSource, MatteToken, register, unregister, warp_to_layer};
use filmcraft_frame::AudioBuffer;
use filmcraft_media::{MediaInfo, MediaKind, MediaSource, VideoStreamInfo};
use filmcraft_project::grade::{Grade, GradeNode};
use filmcraft_project::magic_mask::MagicMask;
use filmcraft_project::{Label, MediaClip, MediaRef, Param, SequenceSettings, TrackKind, resolve_auto_points};
use filmcraft_time::{FrameRate, TimeRange};

const FPS: FrameRate = FrameRate::FPS_24;
const W: usize = 32;
const H: usize = 16;

struct Grey(MediaInfo);

impl MediaSource for Grey {
    fn info(&self) -> &MediaInfo {
        &self.0
    }
    fn video_frame(&self, _req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
        let px = [0.4f32, 0.4, 0.4, 1.0].repeat(W * H);
        Ok(Arc::new(VideoFrame::rgba_f32(W as u32, H as u32, px)))
    }
    fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
        Ok(AudioBuffer::silence(sample_rate, 2, frames))
    }
}

fn info() -> MediaInfo {
    MediaInfo {
        name: "grey".into(),
        kind: MediaKind::Movie,
        duration: FPS.tick_of(4),
        video: Some(VideoStreamInfo {
            camera_color_space: None,
            vfr: None,
            width: W as u32,
            height: H as u32,
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
    }
}

/// A grey clip whose one grade node (id 1, with a Magic Mask) lifts the offset.
fn project() -> (Project, ItemId, SourceMap) {
    let mut p = Project::new("mm");
    let clip = MediaClip {
        media: MediaRef::File { path: "grey.mov".into() },
        info: info(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("grey", Label::Iris, ItemKind::Media(clip), None);
    let size = (W as u32, H as u32);
    let sid = p.new_sequence("s", SequenceSettings { width: size.0, height: size.1, frame_rate: FPS, ..Default::default() }, 1, 0, None);
    let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(4)), FPS).unwrap();
    for e in &mut ti.effects {
        resolve_auto_points(e, size, size);
    }
    let mut n = GradeNode::new(1);
    n.params.insert("offset".into(), Param::new(ParamValue::Color([0.2, 0.2, 0.2, 0.2])));
    n.magic_mask = Some(MagicMask::default());
    ti.grade = Some(Grade { nodes: vec![n], bypass: false, ..Default::default() });
    p.sequence_mut(sid).unwrap().video_tracks[0].items.push(ti);
    let mut map = SourceMap::default();
    map.0.insert(item, Arc::new(Grey(info())) as SharedSource);
    (p, sid, map)
}

/// The left half of the sequence frame, for node 1 only.
struct LeftHalf;

impl MatteSource for LeftHalf {
    fn matte(&self, _p: &Project, _it: &TrackItem, node: &GradeNode, _t: Tick) -> Option<Arc<BitmapMatte>> {
        (node.id == 1).then(|| Arc::new(BitmapMatte::new(2, 1, vec![255, 0]).unwrap()))
    }
}

fn render(p: &Project, seq: ItemId, map: &SourceMap, mattes: MatteToken) -> Image {
    render_sequence(p, seq, Tick::ZERO, RenderOptions { mattes, ..Default::default() }, map)
}

fn red(img: &Image, x: usize, y: usize) -> f32 {
    img.px[(y * img.w + x) * 4]
}

#[test]
fn a_registered_source_limits_its_node_and_without_a_matte_the_node_changes_nothing() {
    let (p, seq, map) = project();
    let mut unmasked = p.clone();
    unmasked.sequence_mut(seq).unwrap().video_tracks[0].items[0].grade.as_mut().unwrap().nodes[0].magic_mask = None;
    let whole = render(&unmasked, seq, &map, MatteToken::NONE);
    let mut plain = p.clone();
    plain.sequence_mut(seq).unwrap().video_tracks[0].items[0].grade = None;
    let ungraded = render(&plain, seq, &map, MatteToken::NONE);
    assert!((red(&whole, 2, 8) - red(&ungraded, 2, 8)).abs() > 0.02, "the node changes the picture");

    let src: Arc<dyn MatteSource> = Arc::new(LeftHalf);
    let token = register(Arc::downgrade(&src));
    let limited = render(&p, seq, &map, token);
    assert!((red(&limited, 2, 8) - red(&whole, 2, 8)).abs() < 1e-4, "inside: graded");
    assert!((red(&limited, W - 3, 8) - red(&ungraded, W - 3, 8)).abs() < 1e-4, "outside: untouched");

    // no token, an unknown one, or the source gone: the node changes nothing (and no crash)
    let same = |img: &Image| img.px.iter().zip(&ungraded.px).all(|(a, b)| (a - b).abs() < 1e-5);
    assert!(same(&render(&p, seq, &map, MatteToken::NONE)));
    assert!(same(&render(&p, seq, &map, MatteToken(u64::MAX))));
    drop(src);
    assert!(same(&render(&p, seq, &map, token)));
    unregister(token);
    assert!(crate::matte::source(token).is_none());
}

#[test]
fn a_matte_is_warped_from_the_sequence_frame_into_the_layer() {
    // one texel per sequence pixel: the left 16 columns inside
    let left = Arc::new(BitmapMatte::new(32, 1, (0..32).map(|x| if x < 16 { 255 } else { 0 }).collect()).unwrap());
    // a layer that fills the frame: the same matte
    let fill = filmcraft_geom::Affine::scale(2.0, 2.0);
    assert!(Arc::ptr_eq(&warp_to_layer(&left, (32.0, 16.0), &fill, 16, 8).unwrap(), &left));
    // a 16 × 8 layer drawn 1:1 on the right half of a 32 × 16 frame: entirely outside the matte
    let right = filmcraft_geom::Affine::translate(16.0, 0.0);
    let w = warp_to_layer(&left, (32.0, 16.0), &right, 16, 8).unwrap();
    assert_eq!((w.width, w.height), (16, 8));
    assert!(w.alpha.iter().all(|a| *a == 0), "{:?}", w.alpha);
    // ...on the left half: entirely inside
    let w = warp_to_layer(&left, (32.0, 16.0), &filmcraft_geom::Affine::IDENTITY, 16, 8).unwrap();
    assert!(w.alpha.iter().all(|a| *a == 255), "{:?}", w.alpha);
    // hostile inputs: None, never a panic
    assert!(warp_to_layer(&left, (0.0, 16.0), &right, 16, 8).is_none());
    assert!(warp_to_layer(&left, (f64::NAN, 16.0), &right, 16, 8).is_none());
    assert!(warp_to_layer(&left, (32.0, 16.0), &right, 0, 8).is_none());
    let nan = filmcraft_geom::Affine { a: f64::NAN, ..filmcraft_geom::Affine::IDENTITY };
    assert!(warp_to_layer(&left, (32.0, 16.0), &nan, 16, 8).is_some_and(|m| m.alpha.iter().all(|a| *a == 0)));
    let huge = warp_to_layer(&left, (32.0, 16.0), &right, 100_000, 10).unwrap();
    assert!(huge.width <= 32, "never finer than the matte: {}", huge.width);
}
