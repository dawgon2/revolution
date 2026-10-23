//! Fork U0.2.3b d: how small an export's own decoders may hand their pictures over. The renderer
//! asks a source for `want` of its size (from the clip's Motion, [`crate::FrameRequest`]), and a
//! source may answer larger, never smaller; so a source only an export reads can have its
//! hardware decoder scale each picture while copying it to the compositing GPU, to the most any
//! clip of the export asks for. Media whose request can change (keyframed Motion), that temporal
//! noise reduction reads at full size, or that a nested sequence or a comp shows, keep their full
//! size, as does all media under Maximum Render Quality (and all media of a sequence where a comp
//! reads a track: it renders the tracks below at the comp's scale).

use crate::RenderOptions;
use filmcraft_project::comp::{CompKind, MediaInSource};
use filmcraft_project::{ItemId, ItemKind, Project, Sequence, TrackItem};

/// The hand-off scale of each media item (resolved: the item a source is opened for) that
/// `seq` rendered with `opts` never asks more than that of; media not listed keep full size.
pub fn handoff_scales(project: &Project, seq: ItemId, opts: RenderOptions) -> Vec<(ItemId, f32)> {
    let mut scales = Vec::new();
    if opts.high_quality {
        return scales;
    }
    let Some(q) = project.sequence(seq) else { return scales };
    for it in q.video_tracks.iter().flat_map(|t| &t.items) {
        match project.item(it.item).map(|p| &p.kind) {
            Some(ItemKind::Sequence(_)) => full_size_inside(project, it.item, 0, &mut scales),
            Some(ItemKind::Comp(_)) => {
                if full_size_in_comp(project, it.item, 0, &mut scales) {
                    // a comp reading a track: those tracks render at the comp's scale (can
                    // be more than their clips ask for): everything keeps its full size
                    return Vec::new();
                }
            }
            _ => {
                if let Some((media, _, _)) = project.resolve_media(it.item) {
                    note(&mut scales, media, item_scale(project, q, it, opts.scale));
                }
            }
        }
    }
    scales.retain(|s| s.1 < 1.0);
    scales
}

/// Every media item a nested sequence shows (at most [`crate::MAX_NEST_DEPTH`] deep) keeps its
/// full size: the nest's own frame size and Motion decide what it asks for.
fn full_size_inside(project: &Project, seq: ItemId, depth: u32, scales: &mut Vec<(ItemId, f32)>) {
    let Some(q) = project.sequence(seq).filter(|_| depth < crate::MAX_NEST_DEPTH) else { return };
    for it in q.video_tracks.iter().flat_map(|t| &t.items) {
        full_size_item(project, it.item, depth.saturating_add(1), scales);
    }
}

/// `item` (media, a nest or a comp) shown at full size.
fn full_size_item(project: &Project, item: ItemId, depth: u32, scales: &mut Vec<(ItemId, f32)>) {
    match project.item(item).map(|p| &p.kind) {
        Some(ItemKind::Sequence(_)) => full_size_inside(project, item, depth, scales),
        // (a track it reads is inside this nest: already full size)
        Some(ItemKind::Comp(_)) => {
            full_size_in_comp(project, item, depth, scales);
        }
        _ => {
            if let Some((media, _, _)) = project.resolve_media(item) {
                note(scales, media, 1.0);
            }
        }
    }
}

/// Every item a comp's Media In nodes read keeps its full size (the comp's own size and Motion
/// decide what it asks for); true when one reads a track of the sequence the comp sits in.
fn full_size_in_comp(project: &Project, comp: ItemId, depth: u32, scales: &mut Vec<(ItemId, f32)>) -> bool {
    let Some(ItemKind::Comp(c)) = project.item(comp).map(|p| &p.kind) else { return false };
    if depth >= crate::MAX_NEST_DEPTH {
        // (too deep to render either)
        return false;
    }
    let mut reads_track = false;
    // (groups' inner nodes included, C4.3)
    for n in c.all_nodes() {
        match &n.kind {
            CompKind::MediaIn { source: MediaInSource::Layer { .. }, .. } => reads_track = true,
            CompKind::MediaIn { source: MediaInSource::Item { item }, .. } => full_size_item(project, *item, depth.saturating_add(1), scales),
            _ => {}
        }
    }
    reads_track
}

fn note(scales: &mut Vec<(ItemId, f32)>, media: ItemId, s: f32) {
    match scales.iter_mut().find(|e| e.0 == media) {
        Some(e) => e.1 = e.1.max(s),
        None => scales.push((media, s)),
    }
}

/// What `it` asks of its source (as `item_layer_ex` computes `want`), or 1.0 when that can
/// change over the clip or something reads the source at full size.
fn item_scale(project: &Project, seq: &Sequence, it: &TrackItem, out_scale: f32) -> f32 {
    if it.temporal_nr.is_some() || it.effect("motion").is_some_and(|m| m.enabled && m.is_animated()) {
        return 1.0;
    }
    let Some(src) = crate::source_size(project, it.item) else { return 1.0 };
    let m = crate::motion_matrix(seq, it, src, crate::source_par(project, it.item), it.start);
    let lin = (m.a * m.a + m.b * m.b).sqrt().max((m.c * m.c + m.d * m.d).sqrt());
    let want = (lin * f64::from(out_scale)).clamp(1.0 / 64.0, 1.0) as f32;
    if want.is_finite() { want } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind, resolve_auto_points};
    use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};

    const FPS: FrameRate = FrameRate::FPS_24;

    fn media(p: &mut Project, name: &str, w: u32, h: u32) -> ItemId {
        let info = filmcraft_media::MediaInfo {
            name: name.into(),
            kind: filmcraft_media::MediaKind::Movie,
            duration: Tick(10 * TICKS_PER_SECOND),
            video: Some(filmcraft_media::VideoStreamInfo {
                camera_color_space: None,
                vfr: None,
                width: w,
                height: h,
                frame_rate: FPS,
                par: (1, 1),
                codec: "test".into(),
                pixel_format: "test".into(),
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
        let clip = MediaClip {
            media: MediaRef::File { path: format!("{name}.mov") },
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        p.add_item(name, Label::Iris, ItemKind::Media(clip), None)
    }

    /// A clip of `item` on V1 of `seq` at `at` seconds, fitted to the frame (`fit`) or at 100 %.
    fn place(p: &mut Project, seq: ItemId, item: ItemId, at: i64, fit: bool) -> usize {
        let (w, h) = p.source_size(item).unwrap();
        let mut ti = p.make_track_item(item, TrackKind::Video, Tick(at * TICKS_PER_SECOND), TimeRange::new(Tick::ZERO, FPS.tick_of(24)), FPS).unwrap();
        let settings = p.sequence(seq).unwrap().settings.clone();
        for e in &mut ti.effects {
            resolve_auto_points(e, (settings.width, settings.height), (w, h));
        }
        ti.scale_to_frame = fit;
        let items = &mut p.sequence_mut(seq).unwrap().video_tracks[0].items;
        items.push(ti);
        items.len() - 1
    }

    fn uhd(p: &mut Project) -> ItemId {
        p.new_sequence("s", SequenceSettings { width: 3840, height: 2160, frame_rate: FPS, ..Default::default() }, 1, 0, None)
    }

    #[test]
    fn a_fitted_6k_clip_in_4k_hands_off_at_the_size_drawn() {
        let mut p = Project::new("h");
        let big = media(&mut p, "big", 6240, 4160);
        let s = uhd(&mut p);
        place(&mut p, s, big, 0, true);
        let got = handoff_scales(&p, s, RenderOptions::default());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, big);
        assert!((got[0].1 - 2160.0 / 4160.0).abs() < 1e-4, "{}", got[0].1);
        // at half output resolution, half that
        let half = handoff_scales(&p, s, RenderOptions { scale: 0.5, ..RenderOptions::default() });
        assert!((half[0].1 - 1080.0 / 4160.0).abs() < 1e-4);
        assert!(handoff_scales(&p, s, RenderOptions { high_quality: true, ..RenderOptions::default() }).is_empty(), "Maximum Render Quality");
    }

    #[test]
    fn the_largest_use_wins_and_full_size_uses_are_not_listed() {
        let mut p = Project::new("h");
        let big = media(&mut p, "big", 6240, 4160);
        let small = media(&mut p, "small", 1920, 1080);
        let s = uhd(&mut p);
        place(&mut p, s, big, 0, true);
        place(&mut p, s, big, 2, false); // 100 %: needs every pixel
        place(&mut p, s, small, 4, false);
        assert!(handoff_scales(&p, s, RenderOptions::default()).is_empty());
    }

    #[test]
    fn keyframed_motion_tnr_and_nests_keep_full_size() {
        let mut p = Project::new("h");
        let big = media(&mut p, "big", 6240, 4160);
        let s = uhd(&mut p);
        let i = place(&mut p, s, big, 0, true);
        let item = &mut p.sequence_mut(s).unwrap().video_tracks[0].items[i];
        let m = item.effect_mut("motion").unwrap();
        let scale = m.params.get_mut("scale").unwrap();
        scale.put_keyframe(Tick::ZERO, filmcraft_project::ParamValue::Float(50.0));
        scale.put_keyframe(Tick(TICKS_PER_SECOND), filmcraft_project::ParamValue::Float(100.0));
        assert!(handoff_scales(&p, s, RenderOptions::default()).is_empty(), "keyframed Motion");

        let mut p = Project::new("h");
        let big = media(&mut p, "big", 6240, 4160);
        let s = uhd(&mut p);
        let i = place(&mut p, s, big, 0, true);
        p.sequence_mut(s).unwrap().video_tracks[0].items[i].temporal_nr = Some(Default::default());
        assert!(handoff_scales(&p, s, RenderOptions::default()).is_empty(), "temporal NR");

        let mut p = Project::new("h");
        let big = media(&mut p, "big", 6240, 4160);
        let inner = uhd(&mut p);
        place(&mut p, inner, big, 0, true);
        let outer = uhd(&mut p);
        place(&mut p, outer, big, 0, true);
        let nest = p.make_track_item(inner, TrackKind::Video, Tick(5 * TICKS_PER_SECOND), TimeRange::new(Tick::ZERO, FPS.tick_of(24)), FPS).unwrap();
        p.sequence_mut(outer).unwrap().video_tracks[0].items.push(nest);
        assert!(handoff_scales(&p, outer, RenderOptions::default()).is_empty(), "also shown in a nest");
        assert_eq!(handoff_scales(&p, ItemId(u64::MAX), RenderOptions::default()), vec![], "no such sequence");
    }

    /// Review 2026-10-15: media a comp reads (Media In of an item, or of a track below the comp)
    /// was handed off at the fitted clip's scale, so the comp got an undersized picture.
    #[test]
    fn media_a_comp_reads_keeps_full_size() {
        use filmcraft_project::comp::{Comp, CompNode};
        use std::sync::Arc;
        let comp_of = |p: &mut Project, source: MediaInSource| {
            let mut c = Comp::new(3840, 2160, FPS, Tick(10 * TICKS_PER_SECOND));
            c.nodes.push(CompNode::new(2, CompKind::MediaIn { source, offset: Tick::ZERO }));
            p.add_item("comp", Label::Iris, ItemKind::Comp(Arc::new(c)), None)
        };
        for source in ["item", "track"] {
            let mut p = Project::new("h");
            let big = media(&mut p, "big", 6240, 4160);
            let s = uhd(&mut p);
            place(&mut p, s, big, 0, true);
            assert_eq!(handoff_scales(&p, s, RenderOptions::default()).len(), 1, "{source}: the fitted clip alone");
            let src = if source == "item" { MediaInSource::Item { item: big } } else { MediaInSource::Layer { track: 0 } };
            let comp = comp_of(&mut p, src);
            let ti = p.make_track_item(comp, TrackKind::Video, Tick(5 * TICKS_PER_SECOND), TimeRange::new(Tick::ZERO, FPS.tick_of(24)), FPS).unwrap();
            p.sequence_mut(s).unwrap().video_tracks[0].items.push(ti);
            assert!(handoff_scales(&p, s, RenderOptions::default()).is_empty(), "{source}: also read by a comp");
        }
    }
}
