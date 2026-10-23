use super::*;

#[test]
fn images_with_impossible_sizes_or_lengths_are_refused() {
    assert!(RgbImage::new(0, 10, vec![]).is_err());
    assert!(RgbImage::new(MAX_SIDE + 1, 1, vec![0; (MAX_SIDE + 1) * 3]).is_err());
    assert!(RgbImage::new(2, 2, vec![0; 11]).is_err());
    assert!(RgbImage::new(usize::MAX, usize::MAX, vec![]).is_err());
    assert!(RgbImage::new(2, 2, vec![0; 12]).is_ok());
}

#[test]
fn prompts_drop_nan_points_clamp_the_rest_and_refuse_empty() {
    let p = Prompt {
        points: vec![Point { x: f32::NAN, y: 0.5, positive: true }, Point { x: 2.0, y: -1.0, positive: false }],
        rect: Some(Rect { x0: 0.9, y0: 0.8, x1: 0.1, y1: f32::INFINITY }),
        prior: None,
        prefer_whole: false,
    };
    let (points, rect) = p.checked().unwrap();
    assert_eq!(points, vec![Point { x: 1.0, y: 0.0, positive: false }]);
    assert_eq!(rect, None);
    assert!(Prompt::default().checked().is_err());
    let many = Prompt { points: vec![Point { x: 0.5, y: 0.5, positive: true }; 1000], ..Prompt::default() };
    assert_eq!(many.checked().unwrap().0.len(), MAX_POINTS);
    let r = Rect { x0: 0.9, y0: 0.8, x1: 0.1, y1: 0.2 }.sorted().unwrap();
    assert_eq!(r, Rect { x0: 0.1, y0: 0.2, x1: 0.9, y1: 0.8 });
}

/// A segment whose subject is the grid square [a, b) on both axes.
fn square(a: usize, b: usize, cover: (f32, f32)) -> Segment {
    let mut s = Segment::empty(cover);
    for y in a..b {
        for x in a..b {
            s.logits[y * LOW_RES + x] = 10.0;
        }
    }
    s
}

#[test]
fn a_segment_scales_to_any_frame_size_within_its_cover() {
    // Subject = left half of the full grid → left half of a 100 × 50 frame.
    let mut s = Segment::empty((1.0, 1.0));
    for y in 0..LOW_RES {
        for x in 0..LOW_RES / 2 {
            s.logits[y * LOW_RES + x] = 5.0;
        }
    }
    let m = s.matte(100, 50).unwrap();
    assert_eq!(m.alpha.iter().filter(|a| **a == 255).count(), 50 * 50);
    assert_eq!(m.alpha[0], 255);
    assert_eq!(m.alpha[99], 0);
    // Cover (1, 0.5): the frame is the top half of the grid (a wide frame padded below).
    let s = square(0, 128, (1.0, 0.5));
    let m = s.matte(64, 32).unwrap();
    assert_eq!(m.alpha.iter().filter(|a| **a == 255).count(), 32 * 32);
    assert!(s.matte(0, 10).is_err());
}

#[test]
fn hostile_segments_never_panic() {
    let short = Segment { logits: vec![1.0; 3], cover: (f32::NAN, 7.0), score: 0.0 };
    let m = short.matte(8, 8).unwrap();
    assert_eq!(m.alpha.len(), 64);
    let nan = Segment { logits: vec![f32::NAN; LOW_RES * LOW_RES], cover: (1.0, 1.0), score: f32::NAN };
    assert!(nan.matte(4, 4).unwrap().alpha.iter().all(|a| *a == 0));
    assert!(follow_prompt(&nan, (0, 0)).is_none());
}

#[test]
fn iou_and_bounds() {
    let a = square(0, 128, (1.0, 1.0)).matte(16, 16).unwrap();
    let b = square(64, 192, (1.0, 1.0)).matte(16, 16).unwrap();
    // 8×8 squares overlapping in 4×4: 16 / (64 + 64 − 16).
    assert!((a.iou(&b).unwrap() - 16.0 / 112.0).abs() < 1e-6);
    assert_eq!(a.iou(&a), Some(1.0));
    assert_eq!(a.iou(&square(0, 1, (1.0, 1.0)).matte(8, 8).unwrap()), None);
    assert_eq!(a.bounds(), Some(Rect { x0: 0.0, y0: 0.0, x1: 0.5, y1: 0.5 }));
    assert_eq!(Segment::empty((1.0, 1.0)).matte(4, 4).unwrap().bounds(), None);
}

#[test]
fn follow_prompt_boxes_the_previous_mask_and_keeps_it_as_prior() {
    let s = square(64, 192, (1.0, 1.0));
    let p = follow_prompt(&s, (1920, 1080)).unwrap();
    let r = p.rect.unwrap();
    assert!(r.x0 < 0.25 && r.x0 > 0.15 && r.x1 > 0.75 && r.x1 < 0.85, "{r:?}");
    assert_eq!(p.prior.as_ref(), Some(&s));
    assert!(p.points.is_empty());
    assert!(follow_prompt(&Segment::empty((1.0, 1.0)), (1920, 1080)).is_none());
}

/// The fake segmenter the engine tests use: the subject is every pixel brighter than 128,
/// cut to the prompt's box.
pub(crate) struct Bright;

impl Segmenter for Bright {
    fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
        Ok(Embedding::new(image.width(), image.height(), Box::new(image.clone())))
    }
    fn segment(&self, e: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError> {
        let img: &RgbImage = e.inner()?;
        let (_, rect) = prompt.checked()?;
        let r = rect.unwrap_or(Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 });
        let mut s = Segment::empty((1.0, 1.0));
        for gy in 0..LOW_RES {
            for gx in 0..LOW_RES {
                let (u, v) = ((gx as f32 + 0.5) / LOW_RES as f32, (gy as f32 + 0.5) / LOW_RES as f32);
                let x = ((u * img.width() as f32) as usize).min(img.width() - 1);
                let y = ((v * img.height() as f32) as usize).min(img.height() - 1);
                let inside = u >= r.x0 && u < r.x1 && v >= r.y0 && v < r.y1 && img.data()[(y * img.width() + x) * 3] > 128;
                s.logits[gy * LOW_RES + gx] = if inside { 10.0 } else { -10.0 };
            }
        }
        s.score = 1.0;
        Ok(s)
    }
}

#[test]
fn a_moving_subject_is_followed_with_the_fake_segmenter() {
    let (w, h) = (64, 32);
    let frame = |x0: usize| {
        let mut d = vec![0u8; w * h * 3];
        for y in 8..24 {
            for x in x0..x0 + 12 {
                d[(y * w + x) * 3] = 255;
            }
        }
        RgbImage::new(w, h, d).unwrap()
    };
    let truth = |x0: usize| {
        let mut a = vec![0u8; w * h];
        for y in 8..24 {
            for x in x0..x0 + 12 {
                a[y * w + x] = 255;
            }
        }
        Matte { width: w, height: h, alpha: a }
    };
    let seg = Bright;
    let first = seg.segment(&seg.embed(&frame(4)).unwrap(), &Prompt { points: vec![Point { x: 0.15, y: 0.5, positive: true }], ..Prompt::default() }).unwrap();
    let mut prev = first;
    for x0 in (6..48).step_by(2) {
        let p = follow_prompt(&prev, (w, h)).unwrap();
        prev = seg.segment(&seg.embed(&frame(x0)).unwrap(), &p).unwrap();
        assert!(prev.matte(w, h).unwrap().iou(&truth(x0)).unwrap() > 0.8);
    }
    // An embedding from another segmenter is refused, not misread.
    let other = Embedding::new(1, 1, Box::new(5u32));
    assert_eq!(seg.segment(&other, &Prompt { rect: Some(Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 }), ..Prompt::default() }), Err(MaskError::WrongEmbedding));
}

/// Track / follow (fork G3) with the fake segmenter: colour-guided carving, a lost subject,
/// hostile inputs, determinism.
mod track_tests {
    use super::Bright;
    use crate::track::{FollowOptions, Track, follow};
    use crate::{Embedding, LOW_RES, MaskError, Matte, Prompt, Rect, RgbImage, Segment, Segmenter};

    const W: usize = 160;
    const H: usize = 90;
    const RED: [u8; 3] = [220, 40, 40];
    const YELLOW: [u8; 3] = [220, 220, 40];

    /// A grey frame with a red subject at x 40..80 and a yellow object (also bright to the fake
    /// segmenter) at x `yx`..`yx + 30`; both y 30..70.
    fn frame(subject: bool, yx: usize) -> RgbImage {
        let mut d = Vec::with_capacity(W * H * 3);
        for y in 0..H {
            for x in 0..W {
                let c = if subject && (40..80).contains(&x) && (30..70).contains(&y) {
                    RED
                } else if (yx..yx + 30).contains(&x) && (30..70).contains(&y) {
                    YELLOW
                } else {
                    [60, 60, 60]
                };
                d.extend_from_slice(&c);
            }
        }
        RgbImage::new(W, H, d).unwrap()
    }

    fn subject() -> Matte {
        Matte { width: W, height: H, alpha: (0..W * H).map(|i| if (40..80).contains(&(i % W)) && (30..70).contains(&(i / W)) { 255 } else { 0 }).collect() }
    }

    /// Frame 0's mask: the stroke box around the subject.
    fn start() -> Track {
        let img = frame(true, 120);
        let e = Bright.embed(&img).unwrap();
        let seg = Bright.segment(&e, &Prompt { rect: Some(Rect { x0: 0.2, y0: 0.25, x1: 0.55, y1: 0.85 }), ..Prompt::default() }).unwrap();
        Track::start(seg, &img).unwrap()
    }

    fn yellow_inside(m: &Matte) -> usize {
        (0..W * H).filter(|i| (83..110).contains(&(i % W)) && (30..70).contains(&(i / W)) && m.alpha[*i] >= 128).count()
    }

    #[test]
    fn the_carve_removes_a_leak_into_a_differently_coloured_object() {
        let t = start();
        // the yellow object (seen in the background on frame 0) now touches the subject
        let img = frame(true, 80);
        let e = Bright.embed(&img).unwrap();
        let off = FollowOptions { carve: false, ..FollowOptions::default() };
        let (raw, rep) = follow(&Bright, &e, &img, &t, &off).unwrap().unwrap();
        assert!(rep.leak > 0.05, "the box lets the fake segmenter leak: {rep:?}");
        assert!(yellow_inside(&raw.segment().matte(W, H).unwrap()) > 100);
        let (carved, rep) = follow(&Bright, &e, &img, &t, &FollowOptions::default()).unwrap().unwrap();
        let m = carved.segment().matte(W, H).unwrap();
        assert_eq!(yellow_inside(&m), 0, "{rep:?}");
        let iou = m.iou(&subject()).unwrap();
        assert!(iou > 0.9, "IoU {iou}");
        // and the next frame starts from the carved mask: the leak doesn't come back
        let (next, _) = follow(&Bright, &e, &img, &carved, &FollowOptions::default()).unwrap().unwrap();
        assert_eq!(yellow_inside(&next.segment().matte(W, H).unwrap()), 0);
    }

    #[test]
    fn a_lost_or_empty_subject_is_a_clear_none() {
        let t = start();
        let gone = frame(false, 120);
        assert!(follow(&Bright, &Bright.embed(&gone).unwrap(), &gone, &t, &FollowOptions::default()).unwrap().is_none());
        assert!(Track::start(Segment::empty((1.0, 1.0)), &gone).is_none());
        assert!(Track::start(Segment { logits: vec![10.0; LOW_RES * LOW_RES], cover: (0.0, 0.0), score: 1.0 }, &gone).is_none());
    }

    #[test]
    fn following_is_deterministic() {
        let t = start();
        let img = frame(true, 80);
        let e = Bright.embed(&img).unwrap();
        let o = FollowOptions { negatives: 2, ..FollowOptions::default() };
        let a = follow(&Bright, &e, &img, &t, &o).unwrap().unwrap();
        let b = follow(&Bright, &e, &img, &t, &o).unwrap().unwrap();
        assert_eq!(a.0.segment(), b.0.segment());
        assert_eq!(a.1, b.1);
    }

    /// A static subject beside a wall of a slightly similar colour (a few of the subject's own
    /// pixels share its colour bin, so it isn't clearly "the rest of the frame"): the box lets
    /// the fake segmenter take a strip of wall each frame; the mask must not creep into it.
    #[test]
    fn the_mask_does_not_creep_into_a_slightly_similar_neighbour() {
        const WALL: [u8; 3] = [180, 120, 90];
        let img = {
            let mut d = Vec::with_capacity(W * H * 3);
            for y in 0..H {
                for x in 0..W {
                    let c = if (40..80).contains(&x) && (30..70).contains(&y) {
                        if (30..32).contains(&y) { WALL } else { RED }
                    } else if (80..120).contains(&x) {
                        WALL
                    } else {
                        [60, 60, 60]
                    };
                    d.extend_from_slice(&c);
                }
            }
            RgbImage::new(W, H, d).unwrap()
        };
        let e = Bright.embed(&img).unwrap();
        let seg = Bright.segment(&e, &Prompt { rect: Some(Rect { x0: 0.25, y0: 0.33, x1: 0.5, y1: 0.78 }), ..Prompt::default() }).unwrap();
        let mut t = Track::start(seg, &img).unwrap();
        for k in 0..30 {
            t = follow(&Bright, &e, &img, &t, &FollowOptions::default()).unwrap().unwrap().0;
            let m = t.segment().matte(W, H).unwrap();
            let wall = (0..W * H).filter(|i| i % W >= 82 && m.alpha[*i] >= 128).count();
            assert_eq!(wall, 0, "frame {k}: {wall} wall pixels");
        }
        let iou = t.segment().matte(W, H).unwrap().iou(&subject()).unwrap();
        assert!(iou > 0.9, "IoU {iou}");
    }

    /// The subject walks into shadow: on one frame all of it takes a darker colour that so far
    /// was only seen elsewhere in the frame. Only its rim may be carved; the inside is kept and
    /// learnt, so the mask recovers over the next frames.
    #[test]
    fn a_real_colour_change_is_learnt_not_carved() {
        const DARK: [u8; 3] = [140, 25, 25];
        let frame = |dark: bool| {
            let mut d = Vec::with_capacity(W * H * 3);
            for y in 0..H {
                for x in 0..W {
                    let c = if (40..80).contains(&x) && (30..70).contains(&y) {
                        if dark { DARK } else { RED }
                    } else if (120..140).contains(&x) && (5..25).contains(&y) {
                        DARK
                    } else {
                        [60, 60, 60]
                    };
                    d.extend_from_slice(&c);
                }
            }
            RgbImage::new(W, H, d).unwrap()
        };
        let lit = frame(false);
        let e = Bright.embed(&lit).unwrap();
        let seg = Bright.segment(&e, &Prompt { rect: Some(Rect { x0: 0.2, y0: 0.25, x1: 0.55, y1: 0.85 }), ..Prompt::default() }).unwrap();
        let mut t = Track::start(seg, &lit).unwrap();
        let dark = frame(true);
        let e = Bright.embed(&dark).unwrap();
        for k in 1..=10 {
            t = follow(&Bright, &e, &dark, &t, &FollowOptions::default()).unwrap().unwrap_or_else(|| panic!("frame {k}: lost")).0;
            let iou = t.segment().matte(W, H).unwrap().iou(&subject()).unwrap();
            assert!(iou > if k == 1 { 0.75 } else { 0.85 }, "frame {k}: IoU {iou}");
        }
    }

    /// A segmenter that answers with garbage.
    enum Hostile {
        Nothing,
        Nan,
        Short,
        BadCover,
        Fails,
    }

    impl Segmenter for Hostile {
        fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
            Ok(Embedding::new(image.width(), image.height(), Box::new(())))
        }
        fn segment(&self, _: &Embedding, _: &Prompt) -> Result<Segment, MaskError> {
            Ok(Segment::empty((1.0, 1.0)))
        }
        fn candidates(&self, _: &Embedding, _: &Prompt, _: bool) -> Result<Vec<Segment>, MaskError> {
            Ok(match self {
                Hostile::Nothing => Vec::new(),
                Hostile::Nan => vec![Segment { logits: vec![f32::NAN; LOW_RES * LOW_RES], cover: (1.0, 1.0), score: f32::NAN }],
                Hostile::Short => vec![Segment { logits: vec![5.0; 3], cover: (1.0, 1.0), score: 1.0 }],
                Hostile::BadCover => vec![Segment { logits: vec![5.0; LOW_RES * LOW_RES], cover: (f32::NAN, f32::INFINITY), score: 1.0 }],
                Hostile::Fails => return Err(MaskError::Failed("boom".into())),
            })
        }
    }

    #[test]
    fn hostile_inputs_never_panic() {
        let t = start();
        let img = frame(true, 80);
        let e = Bright.embed(&img).unwrap();
        for h in [Hostile::Nothing, Hostile::Nan, Hostile::Short, Hostile::BadCover] {
            let r = follow(&h, &e, &img, &t, &FollowOptions::default());
            assert!(r.is_ok(), "{r:?}");
        }
        assert_eq!(follow(&Hostile::Fails, &e, &img, &t, &FollowOptions::default()).err(), Some(MaskError::Failed("boom".into())));
        // absurd options are cleaned, not obeyed
        let wild = FollowOptions {
            positives: usize::MAX,
            negatives: usize::MAX,
            carve_min: usize::MAX,
            carve_grow: usize::MAX,
            accept: f32::NAN,
            grow: f32::INFINITY,
            grow_min: -5.0,
            prior: true,
            multimask: true,
            carve: true,
        };
        let c = wild.cleaned();
        assert!(c.positives <= 8 && c.negatives <= 8 && c.carve_grow <= crate::track::MAX_CARVE_GROW);
        assert!(c.accept.is_finite() && (0.0..=1.0).contains(&c.grow) && c.grow_min == 0.0);
        assert!(follow(&Bright, &e, &img, &t, &wild).is_ok());
        let none = FollowOptions { positives: 0, negatives: 0, ..FollowOptions::default() };
        let (_, rep) = follow(&Bright, &e, &img, &t, &none).unwrap().unwrap();
        assert!(rep.positives.is_empty());
        // frames of odd sizes (1 pixel, very wide, very tall, a different size than the start)
        for (w, h) in [(1, 1), (16_384, 1), (1, 16_384), (3, 2000), (W * 2, H / 2)] {
            let img = RgbImage::new(w, h, vec![200; w * h * 3]).unwrap();
            let e = Bright.embed(&img).unwrap();
            assert!(follow(&Bright, &e, &img, &t, &FollowOptions::default()).is_ok(), "{w}×{h}");
            let all = Segment { logits: vec![10.0; LOW_RES * LOW_RES], cover: (1.0, 1.0), score: 1.0 };
            if let Some(t2) = Track::start(all, &img) {
                assert!(follow(&Bright, &e, &img, &t2, &FollowOptions::default()).is_ok(), "{w}×{h}");
            }
        }
    }
}

/// The installed MobileSAM (`$FILMCRAFT_MOBILESAM`, else `models/mobilesam/mobile_sam.pt` in
/// the checkout), or `None` (the test skips) when it isn't there.
#[cfg(feature = "sam")]
pub(crate) fn real_model() -> Option<crate::sam::MobileSam> {
    let model = std::env::var_os("FILMCRAFT_MOBILESAM")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/mobilesam/mobile_sam.pt"));
    if !model.is_file() {
        eprintln!("skipped: {} not installed", model.display());
        return None;
    }
    Some(crate::sam::MobileSam::load(&model).unwrap())
}

#[cfg(feature = "sam")]
mod sam_tests {
    use crate::MaskError;
    use crate::sam::MobileSam;

    /// The encoder's tensors come out as plain data, and an embedding made from the CPU
    /// encoder's own output decodes exactly like `embed`; bad features are refused.
    #[test]
    fn encoder_weights_and_embeddings_from_plain_features() {
        use crate::sam::EMBEDDING_LEN;
        use crate::{Prompt, Rect, RgbImage, Segmenter};
        let Some(sam) = crate::tests::real_model() else { return };
        let w = sam.encoder_weights().unwrap();
        let qkv = w.iter().find(|t| t.name == "layers.1.blocks.0.attn.qkv.weight").unwrap();
        assert_eq!(qkv.shape, vec![384, 128]);
        assert_eq!(qkv.data.len(), 384 * 128);
        assert!(w.iter().all(|t| t.data.len() == t.shape.iter().product::<usize>()));
        assert!(w.iter().any(|t| t.name == "neck.3.bias"));

        let (iw, ih) = (200, 120);
        let img = RgbImage::new(iw, ih, (0..iw * ih * 3).map(|i| (i * 7 % 253) as u8).collect()).unwrap();
        let (input, fitted) = sam.prepare_input(&img).unwrap();
        assert_eq!(fitted, (1024, 614));
        let feats = sam.encode_cpu(&input).unwrap();
        assert_eq!(feats.len(), EMBEDDING_LEN);
        let p = Prompt { rect: Some(Rect { x0: 0.2, y0: 0.2, x1: 0.6, y1: 0.7 }), ..Prompt::default() };
        let a = sam.segment(&sam.embed(&img).unwrap(), &p).unwrap();
        let b = sam.segment(&sam.embedding_from_features(feats.clone(), iw, ih, fitted).unwrap(), &p).unwrap();
        assert_eq!(a, b);
        assert!(sam.embedding_from_features(feats[1..].to_vec(), iw, ih, fitted).is_err());
        let mut nan = feats.clone();
        nan[5] = f32::NAN;
        assert!(sam.embedding_from_features(nan, iw, ih, fitted).is_err());
        assert!(sam.embedding_from_features(feats.clone(), iw, ih, (0, 5)).is_err());
        assert!(sam.embedding_from_features(feats, 0, ih, fitted).is_err());
        assert!(sam.encode_cpu(&[0.0; 12]).is_err());
    }

    #[test]
    fn a_missing_or_corrupt_model_says_why() {
        let dir = std::env::temp_dir().join(format!("fc-mask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(MobileSam::load(&dir.join("nope.pt")), Err(MaskError::ModelMissing(_))));
        assert!(matches!(MobileSam::load(&dir), Err(MaskError::ModelMissing(_))));
        let small = dir.join("small.pt");
        std::fs::write(&small, b"not a model").unwrap();
        assert!(matches!(MobileSam::load(&small), Err(MaskError::ModelCorrupt(_))));
        // Right size, garbage content, and a zip that isn't a state dict.
        let junk = dir.join("junk.pt");
        std::fs::write(&junk, (0..2u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect::<Vec<_>>()).unwrap();
        let e = MobileSam::load(&junk).unwrap_err();
        assert!(matches!(e, MaskError::ModelCorrupt(_)), "{e:?}");
        assert!(!e.to_string().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A click + box prompt on a plain square (regression: candle 0.9's own box embedding is
    /// rank 2 and failed when joined with points). Skips when the model isn't installed.
    #[test]
    fn a_click_and_box_find_a_square_with_the_real_model() {
        use crate::{Point, Prompt, Rect, RgbImage, Segmenter};
        let Some(sam) = crate::tests::real_model() else { return };
        let (w, h) = (320, 180);
        let inside = |x: usize, y: usize| (120..200).contains(&x) && (50..130).contains(&y);
        let mut d = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                d.extend_from_slice(if inside(x, y) { &[220, 60, 40] } else { &[40, 90, 160] });
            }
        }
        let img = RgbImage::new(w, h, d).unwrap();
        let truth = crate::Matte { width: w, height: h, alpha: (0..w * h).map(|i| if inside(i % w, i / w) { 255 } else { 0 }).collect() };
        let e = sam.embed(&img).unwrap();
        let prompt = Prompt {
            points: vec![Point { x: 0.5, y: 0.5, positive: true }],
            rect: Some(Rect { x0: 0.35, y0: 0.25, x1: 0.65, y1: 0.75 }),
            prior: None,
            prefer_whole: false,
        };
        let seg = sam.segment(&e, &prompt).unwrap();
        let iou = seg.matte(w, h).unwrap().iou(&truth).unwrap();
        assert!(iou > 0.9, "IoU {iou}");
        // The box alone and the result as the next prior work too.
        let follow = crate::follow_prompt(&seg, (w, h)).unwrap();
        assert!(sam.segment(&e, &follow).unwrap().matte(w, h).unwrap().iou(&truth).unwrap() > 0.9);
    }

    /// Our box embedding equals candle's (whose only fault is its rank), a prior-only prompt
    /// works, and bad priors / a zero-area box are refused or handled. Skips without the model.
    #[test]
    fn box_embedding_matches_candle_and_odd_prompts_never_panic() {
        use crate::{LOW_RES, Prompt, Rect, RgbImage, Segment, Segmenter};
        let Some(sam) = crate::tests::real_model() else { return };
        let (theirs, ours) = sam.candle_box_and_ours([100.0, 200.0, 700.0, 900.5]).unwrap();
        assert_eq!(theirs.len(), ours.len());
        assert!(theirs.iter().zip(&ours).all(|(a, b)| (a - b).abs() < 1e-4));

        let (w, h) = (64, 64);
        let img = RgbImage::new(w, h, (0..w * h * 3).map(|i| (i % 251) as u8).collect()).unwrap();
        let e = sam.embed(&img).unwrap();
        let mut prior = Segment::empty((1.0, 1.0));
        prior.logits[LOW_RES * 128 + 128] = f32::NAN;
        prior.logits[0] = f32::INFINITY;
        assert!(sam.segment(&e, &Prompt { prior: Some(prior.clone()), ..Prompt::default() }).is_ok());
        let short = Segment { logits: vec![0.0; 7], ..prior.clone() };
        assert!(matches!(sam.segment(&e, &Prompt { prior: Some(short), ..Prompt::default() }), Err(MaskError::BadPrompt(_))));
        let other_shape = Segment::empty((1.0, 0.5));
        assert!(matches!(sam.segment(&e, &Prompt { prior: Some(other_shape), ..Prompt::default() }), Err(MaskError::BadPrompt(_))));
        let flat = Rect { x0: 0.5, y0: 0.2, x1: 0.5, y1: 0.8 };
        assert!(sam.segment(&e, &Prompt { rect: Some(flat), ..Prompt::default() }).is_ok());
        // A wide frame shrinks by area averaging and still segments.
        let wide = RgbImage::new(3000, 20, vec![90; 3000 * 20 * 3]).unwrap();
        let e = sam.embed(&wide).unwrap();
        assert!(sam.segment(&e, &Prompt { rect: Some(Rect { x0: 0.1, y0: 0.0, x1: 0.2, y1: 1.0 }), ..Prompt::default() }).is_ok());
    }
}

/// A segmenter whose candidates for an ambiguous prompt differ in extent: a part (the best
/// score), the whole subject (a little lower), and a spill into the background (the largest,
/// clearly lower); a single answer otherwise. It answers through [`segment_by_candidates`], as
/// MobileSAM does, and records whether it was asked for several candidates.
struct Parts {
    asked: std::sync::Mutex<Vec<bool>>,
}

/// A segment whose first `n` grid columns are inside.
fn cols(n: usize, score: f32) -> Segment {
    let mut s = Segment::empty((1.0, 1.0));
    for (i, v) in s.logits.iter_mut().enumerate() {
        *v = if i % LOW_RES < n { 10.0 } else { -10.0 };
    }
    s.score = score;
    s
}

fn inside_cols(s: &Segment) -> usize {
    s.logits.iter().filter(|v| **v > 0.0).count() / LOW_RES
}

impl Segmenter for Parts {
    fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
        Ok(Embedding::new(image.width(), image.height(), Box::new(())))
    }
    fn segment(&self, e: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError> {
        segment_by_candidates(self, e, prompt)
    }
    fn candidates(&self, _: &Embedding, _: &Prompt, multimask: bool) -> Result<Vec<Segment>, MaskError> {
        self.asked.lock().unwrap().push(multimask);
        Ok(if multimask { vec![cols(40, 0.95), cols(120, 0.9), cols(250, 0.5)] } else { vec![cols(80, 0.97)] })
    }
}

#[test]
fn person_mode_keeps_the_whole_figure_object_mode_the_best_scored() {
    let seg = Parts { asked: std::sync::Mutex::new(Vec::new()) };
    let e = seg.embed(&RgbImage::new(8, 8, vec![0; 8 * 8 * 3]).unwrap()).unwrap();
    let click = Prompt { points: vec![Point { x: 0.3, y: 0.5, positive: true }], ..Prompt::default() };
    let person = |p: &Prompt| Prompt { prefer_whole: true, ..p.clone() };
    // one click (ambiguous): Object keeps the best-scored part, Person the whole figure, never
    // the spill that scores far below
    assert_eq!(inside_cols(&seg.segment(&e, &click).unwrap()), 40);
    assert_eq!(inside_cols(&seg.segment(&e, &person(&click)).unwrap()), 120);
    // several points without a box: Object asks for one mask (as before), Person for the
    // candidates
    let two = Prompt { points: vec![Point { x: 0.3, y: 0.5, positive: true }, Point { x: 0.4, y: 0.6, positive: true }], ..Prompt::default() };
    seg.asked.lock().unwrap().clear();
    assert_eq!(inside_cols(&seg.segment(&e, &two).unwrap()), 80);
    assert_eq!(inside_cols(&seg.segment(&e, &person(&two)).unwrap()), 120);
    assert_eq!(*seg.asked.lock().unwrap(), vec![false, true]);
    // a box or a previous mask fixes the extent: Person changes nothing
    let boxed = Prompt { rect: Some(Rect { x0: 0.1, y0: 0.1, x1: 0.9, y1: 0.9 }), ..person(&click) };
    assert_eq!(inside_cols(&seg.segment(&e, &boxed).unwrap()), 80);
    let prior = Prompt { prior: Some(cols(60, 1.0)), ..person(&click) };
    assert_eq!(inside_cols(&seg.segment(&e, &prior).unwrap()), 80);
}

#[test]
fn candidate_choice_follows_the_margin_and_ignores_bad_scores() {
    assert_eq!(choose_candidate(&[], true), None);
    assert_eq!(choose_candidate(&[], false), None);
    let c = [(0.95, 10), (0.9, 30), (0.5, 90)];
    assert_eq!(choose_candidate(&c, false), Some(0));
    assert_eq!(choose_candidate(&c, true), Some(1));
    // just outside the margin: not taken
    let c = [(0.95, 10), (0.95 - WHOLE_MARGIN - 0.01, 30)];
    assert_eq!(choose_candidate(&c, true), Some(0));
    // equal areas: the better score; equal scores: the first
    assert_eq!(choose_candidate(&[(0.9, 30), (0.93, 30)], true), Some(1));
    assert_eq!(choose_candidate(&[(0.9, 30), (0.9, 10)], false), Some(0));
    // NaN / infinite scores never win; all bad: the first
    let c = [(f32::NAN, 99), (0.8, 5), (f32::INFINITY, 100), (0.75, 20)];
    assert_eq!(choose_candidate(&c, false), Some(1));
    assert_eq!(choose_candidate(&c, true), Some(3));
    assert_eq!(choose_candidate(&[(f32::NAN, 1), (f32::NAN, 2)], true), Some(0));
}
