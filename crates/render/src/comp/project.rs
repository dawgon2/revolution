//! A comp clip's outside pictures from the project: Media In items (media, subclips, nested
//! sequences, other comps) and timeline layers below the comp clip.

use filmcraft_geom::Affine;
use filmcraft_media::FrameRequest;
use filmcraft_project::comp::MediaInSource;
use filmcraft_project::{ItemId, ItemKind, Project, Sequence};
use filmcraft_time::Tick;

use super::{CompCtx, CompSources};
use crate::image::Image;
use crate::{MAX_NEST_DEPTH, RenderOptions, SourceProvider, decimation, render_seq_tracks};

/// Pictures for a comp clip on video track `track` of `seq`.
pub(crate) struct ProjectSources<'a> {
    pub project: &'a Project,
    pub seq: &'a Sequence,
    /// The comp clip's video track: a Layer source must be below it (or it would draw itself).
    pub track: Option<usize>,
    pub opts: RenderOptions,
    pub sources: &'a dyn SourceProvider,
}

/// `img` scaled to fit a `w`×`h` canvas, centred, transparent around it.
pub(crate) fn fit(img: &Image, w: usize, h: usize) -> Image {
    if (img.w, img.h) == (w, h) {
        return img.clone();
    }
    let s = (w as f64 / img.w.max(1) as f64).min(h as f64 / img.h.max(1) as f64);
    let (ox, oy) = ((w as f64 - img.w as f64 * s) / 2.0, (h as f64 - img.h as f64 * s) / 2.0);
    img.transformed(w, h, &Affine::translate(ox, oy).then_apply(&Affine::scale(s, s)))
}

impl ProjectSources<'_> {
    fn nested(&self) -> RenderOptions {
        RenderOptions { depth: self.opts.depth.saturating_add(1), captions: false, working_output: true, ..self.opts }
    }

    /// A media item's or subclip's frame at `t`, decoded at the size it is drawn.
    fn item_frame(&self, item: ItemId, t: Tick, w: usize, h: usize) -> Option<(std::sync::Arc<filmcraft_frame::VideoFrame>, f32, u32)> {
        let pi = self.project.item(item)?;
        let time = match &pi.kind {
            ItemKind::Subclip { range, .. } => Tick(range.start.0.saturating_add(t.0)),
            ItemKind::Media(_) => t,
            _ => return None,
        };
        let (sw, sh) = self.project.source_size(item)?;
        let want = (w as f32 / sw.max(1) as f32).min(h as f32 / sh.max(1) as f32).clamp(0.01, 1.0);
        let src = self.sources.source(item)?;
        let frame = src.video_frame(FrameRequest { time, scale: want }).ok()?;
        Some((frame, want, sw))
    }

    /// A media item's or subclip's frame as one GPU layer (C1 owed: Item-source Media In drew a
    /// CPU picture); None for other items and RGB frames that need colour management.
    fn item_layer(&self, item: ItemId, t: Tick, w: usize, h: usize) -> Option<crate::plan::PlanLayer> {
        let (frame, _, _) = self.item_frame(item, t, w, h)?;
        crate::plan::media_frame_layer(self.project, item, frame, &self.seq.settings.color, w, h)
    }
}

impl CompSources for ProjectSources<'_> {
    fn media_in_layers(&self, source: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Vec<crate::plan::PlanLayer>> {
        if self.opts.depth >= MAX_NEST_DEPTH {
            return None;
        }
        let track = match *source {
            MediaInSource::Layer { track } => track,
            MediaInSource::Item { item } => return self.item_layer(item, t, w, h).map(|l| vec![l]),
            // (C4.3: a group's inputs are read by the comp evaluator itself)
            MediaInSource::GroupInput { .. } => return None,
        };
        if self.track.is_none_or(|own| (track as usize) >= own) {
            return None;
        }
        // the sequence fitted into the comp canvas, as `fit` places the CPU picture
        let (sw, sh) = (f64::from(self.seq.settings.width.max(1)), f64::from(self.seq.settings.height.max(1)));
        let s = (w as f64 / sw).min(h as f64 / sh);
        if !s.is_finite() || s <= 0.0 {
            return None;
        }
        let (ox, oy) = ((w as f64 - sw * s) / 2.0, (h as f64 - sh * s) / 2.0);
        let opts = RenderOptions { scale: s as f32, ..self.nested() };
        let mut layers = crate::plan::track_layers(self.project, self.seq, track as usize, t, opts, self.sources)?;
        let shift = Affine::translate(ox, oy);
        for l in &mut layers {
            l.matrix = shift.then_apply(&l.matrix);
        }
        Some(layers)
    }

    fn nest_depth(&self) -> u32 {
        self.opts.depth
    }

    fn media_in(&self, source: &MediaInSource, t: Tick, w: usize, h: usize) -> Option<Image> {
        if self.opts.depth >= MAX_NEST_DEPTH {
            return None;
        }
        match *source {
            MediaInSource::Layer { track } => {
                // only tracks below the comp clip's own (a comp over the tracks beneath it)
                if self.track.is_none_or(|own| (track as usize) >= own) || self.seq.video_tracks.get(track as usize).is_none() {
                    return None;
                }
                let img = render_seq_tracks(self.project, self.seq, t, self.nested(), self.sources, Some(track as usize));
                Some(fit(&img, w, h))
            }
            MediaInSource::Item { item } => {
                let pi = self.project.item(item)?;
                let img = match &pi.kind {
                    ItemKind::Sequence(q) => {
                        let scale = (w as f32 / q.settings.width.max(1) as f32).min(h as f32 / q.settings.height.max(1) as f32);
                        render_seq_tracks(self.project, q, t, RenderOptions { scale: scale.clamp(0.01, 1.0), ..self.nested() }, self.sources, None)
                    }
                    ItemKind::Comp(c) => {
                        let inner = ProjectSources { track: None, opts: self.nested(), ..*self };
                        let ctx = CompCtx { t, size: (w, h), project: Some(self.project), high_quality: self.opts.high_quality };
                        super::render_cpu(c, ctx, &inner)
                    }
                    ItemKind::Media(_) | ItemKind::Subclip { .. } => {
                        let (frame, want, sw) = self.item_frame(item, t, w, h)?;
                        let n = decimation(frame.width as f32, sw as f32 * want);
                        crate::colorman::decode(self.project, item, &frame, n, &self.seq.settings.color)
                    }
                    ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => return None,
                };
                Some(fit(&img, w, h))
            }
            MediaInSource::GroupInput { .. } => None,
        }
    }
}
