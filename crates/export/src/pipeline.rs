//! The per-frame picture pipeline shared by every export format: render the sequence at the
//! output frame rate, fit it into the output frame (scale to fit / fill / stretch, pixel aspect),
//! apply the Export effects (image, name and timecode overlays, video limiter) and convert to the
//! encoder's input (straight sRGB RGBA8, or encoded R'G'B' floats for HDR).

use std::sync::Arc;

use filmcraft_geom::Vec2;
use filmcraft_project::{ItemId, Project};
use filmcraft_render::{Image, RenderOptions, SourceProvider};
use filmcraft_time::FrameRate;
use rayon::prelude::*;

use crate::settings::{ExportEffects, Placement, Scaling, TextOverlay};
use crate::{ExportError, ExportSettings, FrameRenderer, GpuRendering, Result, build_frame_renderer, note_gpu_fallback, note_gpu_frame};

/// Where the rendered picture lands in the output frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    pub w: u32,
    pub h: u32,
    /// Render scale (relative to the sequence frame size).
    pub render_scale: f32,
    /// Size of the picture in the output frame (None = as rendered).
    pub content: Option<(usize, usize)>,
    /// Top-left of the picture in the output frame (negative = cropped).
    pub offset: (i64, i64),
}

impl Geometry {
    pub fn new(settings: &ExportSettings, seq_w: u32, seq_h: u32, w: u32, h: u32) -> Self {
        let (sw, sh) = (seq_w.max(1) as f32, seq_h.max(1) as f32);
        if settings.frame_size.is_none() && settings.pixel_aspect.is_none() && !settings.max_render_quality {
            // the sequence size times `scale`, rendered directly (even-size crop/pad at the edge)
            return Geometry { w, h, render_scale: w as f32 / sw, content: None, offset: (0, 0) };
        }
        let par = settings.pixel_aspect.map(|(n, d)| n.max(1) as f32 / d.max(1) as f32).unwrap_or(1.0);
        let disp_w = w as f32 * par;
        let (fx, fy) = (disp_w / sw, h as f32 / sh);
        let (sx, sy) = match settings.scaling {
            Scaling::ScaleToFit => (fx.min(fy), fx.min(fy)),
            Scaling::ScaleToFill => (fx.max(fy), fx.max(fy)),
            Scaling::StretchToFill => (fx, fy),
        };
        let cw = (sw * sx / par).round().max(1.0) as usize;
        let ch = (sh * sy).round().max(1.0) as usize;
        let render_scale = if settings.max_render_quality { 1.0f32.max(sy).max(sx / par) } else { sy.max(sx / par) }.clamp(0.01, 4.0);
        Geometry { w, h, render_scale, content: Some((cw, ch)), offset: ((w as i64 - cw as i64) / 2, (h as i64 - ch as i64) / 2) }
    }
}

/// GPU renderers shared by the export workers: a worker takes a free one for a frame and puts it
/// back (a single renderer behind a mutex made the workers queue, GPU export ~20 % slower than CPU).
struct RendererPool {
    renderers: std::sync::Mutex<Vec<Box<dyn FrameRenderer>>>,
    available: std::sync::Condvar,
    /// A renderer panicked: the pool is retired and the rest of the export renders on the CPU.
    failed: std::sync::atomic::AtomicBool,
}

impl RendererPool {
    /// `None` for an empty list (a pool nobody could take from).
    fn new(renderers: Vec<Box<dyn FrameRenderer>>) -> Option<Self> {
        (!renderers.is_empty()).then(|| Self {
            renderers: std::sync::Mutex::new(renderers),
            available: std::sync::Condvar::new(),
            failed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Run `f` with a free renderer, waiting for one if all are busy. `None` once a renderer has
    /// panicked: the panicking renderer is dropped (its state can't be trusted) and so are the
    /// others as they come back, so the caller falls back to the CPU for the rest of the export.
    fn with<R>(&self, f: impl FnOnce(&mut dyn FrameRenderer) -> R) -> Option<R> {
        use std::sync::atomic::Ordering;
        let mut free = self.renderers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut r = loop {
            if self.failed.load(Ordering::SeqCst) {
                return None;
            }
            match free.pop() {
                Some(r) => break r,
                None => free = self.available.wait(free).unwrap_or_else(std::sync::PoisonError::into_inner),
            }
        };
        drop(free);
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *r)));
        let mut free = self.renderers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if res.is_err() {
            // (the panic hook has logged the panic itself)
            self.failed.store(true, Ordering::SeqCst);
            free.clear();
            drop(free);
            drop(r);
        } else if self.failed.load(Ordering::SeqCst) {
            drop(free);
            drop(r);
        } else {
            free.push(r);
            drop(free);
        }
        // waiters re-check `failed` (or take the renderer that came back)
        self.available.notify_all();
        res.ok()
    }
}

/// The immutable part of an export: everything needed to produce one output frame.
pub(crate) struct Pipeline {
    pub project: Arc<Project>,
    /// The registered frame renderers (GPU compositors) when the setting allows and factories
    /// provide them; pooled because `frame` runs on several export threads at once.
    renderer: Option<RendererPool>,
    pub seq: ItemId,
    /// Output frame rate (frame `f` is at `rate.tick_of(f)`).
    pub rate: FrameRate,
    seq_rate: FrameRate,
    pub w: u32,
    pub h: u32,
    geom: Geometry,
    pub(crate) opts: RenderOptions,
    pub hdr_out: bool,
    /// Keep straight alpha in the RGBA8 output instead of flattening over black.
    alpha: bool,
    out_tf: Option<filmcraft_color::OutputTransform>,
    effects: ExportEffects,
    overlay: Option<Image>,
    start_tc: i64,
    drop_frame: bool,
    /// Free the pool's float images when the export ends: a standalone export, not one part of a
    /// batch (`ExportSettings::part_of_batch`).
    trim_pool: bool,
    /// The GPU frame renderer of a two-GPU export's lane (fork 6E.3); `None`: the registered one
    /// ([`crate::gpu_frames`]).
    gpu: Option<Arc<dyn crate::GpuFrames>>,
}

impl Drop for Pipeline {
    /// The export is over, however it ended (done, failed, cancelled, dropped half way). The float
    /// images its frames left on the `filmcraft_frame::pool` float shelf are the size of its
    /// frames, which nothing else asks for, so free them instead of holding up to 320 MiB idle. A
    /// job still rendering at that moment allocates a few images again; the pool is only a cache.
    /// Parts of a larger job (render-preview segments, proxies) keep them for the next part.
    fn drop(&mut self) {
        if self.trim_pool {
            filmcraft_frame::pool::trim_f32();
        }
    }
}

impl Pipeline {
    pub fn new(project: Arc<Project>, seq: ItemId, settings: &ExportSettings, hdr_out: bool) -> Result<Self> {
        let q = project.sequence(seq).ok_or(ExportError::NoSequence)?;
        q.settings.validate().map_err(ExportError::Unsupported)?;
        settings.validate()?;
        let r = settings.resolve(q.settings.width, q.settings.height, q.settings.frame_rate, q.settings.sample_rate);
        filmcraft_project::validate_frame_size(r.width, r.height).map_err(ExportError::Unsupported)?;
        let geom = Geometry::new(settings, q.settings.width, q.settings.height, r.width, r.height);
        if let Some((w, h)) = geom.content {
            let w = u32::try_from(w).map_err(|_| ExportError::Unsupported("scaled picture width is too large".into()))?;
            let h = u32::try_from(h).map_err(|_| ExportError::Unsupported("scaled picture height is too large".into()))?;
            filmcraft_project::validate_frame_size(w, h).map_err(ExportError::Unsupported)?;
        }
        let (w, h) = filmcraft_render::output_size(q, geom.render_scale);
        let w = u32::try_from(w).map_err(|_| ExportError::Unsupported("rendered picture width is too large".into()))?;
        let h = u32::try_from(h).map_err(|_| ExportError::Unsupported("rendered picture height is too large".into()))?;
        filmcraft_project::validate_frame_size(w, h).map_err(ExportError::Unsupported)?;
        let pipe = q.settings.color;
        let out_tf = hdr_out.then(|| filmcraft_color::OutputTransform::new(&pipe, pipe.working.output_space()));
        let opts = RenderOptions {
            scale: geom.render_scale,
            captions: settings.burn_captions,
            working_output: hdr_out,
            high_quality: settings.max_render_quality || q.settings.max_render_quality,
            // temporal noise reduction (fork 6.7) renders here: every export, and so every
            // render-cache part (they are exports of a segment)
            temporal: true,
            ..Default::default()
        };
        let effects = settings.effects.clone();
        let overlay = if effects.image_overlay.enabled && !effects.image_overlay.path.is_empty() {
            Some(load_overlay(&effects.image_overlay.path, r.width as f32 * effects.image_overlay.size_percent.clamp(0.5, 100.0) / 100.0)?)
        } else {
            None
        };
        let renderer = if settings.gpu_rendering == GpuRendering::Off {
            None
        } else {
            // Four renderers: as fast as one per worker (measured on 16 threads, where more only
            // contend for memory bandwidth in the read-back), with a quarter of the GPU memory.
            let count = rayon::current_num_threads().clamp(2, 4);
            let mut list = Vec::with_capacity(count);
            for _ in 0..count {
                if let Some(r) = build_frame_renderer() {
                    list.push(r);
                } else {
                    break;
                }
            }
            RendererPool::new(list)
        };
        Ok(Pipeline {
            renderer,
            seq_rate: q.settings.frame_rate,
            start_tc: q.start_timecode,
            drop_frame: q.settings.drop_frame,
            trim_pool: !settings.part_of_batch,
            project: project.clone(),
            seq,
            rate: r.rate,
            w: r.width,
            h: r.height,
            geom,
            opts,
            hdr_out,
            alpha: settings.keeps_alpha(),
            out_tf,
            effects,
            overlay,
            gpu: None,
        })
    }

    /// This pipeline for a lane of a two-GPU export (fork 6E.3): frames render on `gpu` (`None`:
    /// the registered renderer), and the frame pool is left to the exporter's own pipeline.
    pub fn for_lane(mut self, gpu: Option<Arc<dyn crate::GpuFrames>>) -> Self {
        self.gpu = gpu;
        self.trim_pool = false;
        self
    }

    /// The GPU frame renderer this pipeline uses, unless `FILMCRAFT_EXPORT_CPU` is set.
    pub fn gpu(&self) -> Option<&dyn crate::GpuFrames> {
        match &self.gpu {
            Some(g) if std::env::var_os("FILMCRAFT_EXPORT_CPU").is_none() => Some(g.as_ref()),
            Some(_) => None,
            None => crate::gpu_frames(),
        }
    }

    /// Output frame `f` as straight sRGB RGBA8 at the output size, or for HDR exports the encoded
    /// R'G'B' floats (3 per pixel).
    pub fn frame(&self, f: i64, sources: &dyn SourceProvider) -> (Vec<u8>, Vec<f32>) {
        let t = self.rate.tick_of(f);
        // The registered GPU renderer goes first; `None` (no adapter, a plan the GPU cannot
        // draw, any internal error) falls back to the CPU reference renderer below. A frame the
        // planner hands back as one CPU image anyway (an adjustment layer, a complex transition,
        // HDR) does not take a pooled renderer: it would render on the CPU inside the pool, as
        // many frames at a time as there are renderers instead of one per export thread.
        let pool = self.renderer.as_ref().filter(|_| !filmcraft_render::plan::is_cpu_frame(&self.project, self.seq, t));
        let gpu = match pool {
            Some(pool) => {
                let asked = web_time::Instant::now();
                pool.with(|r| {
                    crate::note_lock_wait(asked.elapsed());
                    r.render(&self.project, self.seq, t, self.opts, sources)
                })
                .flatten()
            }
            None => None,
        };
        let img = match gpu {
            Some(img) => {
                note_gpu_frame();
                img
            }
            None => {
                if self.renderer.is_some() {
                    note_gpu_fallback();
                }
                filmcraft_render::render_sequence(&self.project, self.seq, t, self.opts, sources)
            }
        };
        let mut img = self.place(img);
        self.overlays(&mut img, t);
        let lim = &self.effects.video_limiter;
        if let Some(tf) = self.out_tf.as_ref().filter(|_| self.hdr_out) {
            let mut out = vec![0f32; img.w * img.h * 3];
            out.par_chunks_mut(img.w * 3).zip(img.px.par_chunks(img.w * 4)).for_each(|(o, s)| {
                for (o, p) in o.as_chunks_mut::<3>().0.iter_mut().zip(s.as_chunks::<4>().0) {
                    o.copy_from_slice(&tf.encode([p[0], p[1], p[2]]));
                }
            });
            if lim.enabled {
                limit_f32(&mut out, lim.min_percent, lim.max_percent);
            }
            // the float image is not needed any more: its buffer serves the next frame's layers
            filmcraft_frame::pool::recycle_f32(img.px);
            return (Vec::new(), out);
        }
        let mut rgba = if self.alpha { img.to_rgba8() } else { img.over_black_rgba8() };
        // the float image is not needed any more: its buffer serves the next frame's layers
        filmcraft_frame::pool::recycle_f32(img.px);
        if lim.enabled {
            limit_rgba8(&mut rgba, lim.min_percent, lim.max_percent);
        }
        (rgba, Vec::new())
    }

    /// Output frame `f` rendered and converted on the GPU ([`crate::GpuFrames`]) as planar 4:2:0
    /// (`yuv420p10le` when `ten_bit`), when this export allows it: SDR, the sequence rendered at
    /// exactly the output size, no burn-ins, overlays or limiter (those are CPU steps). None:
    /// use [`frame`](Self::frame). With `target` (the encoder's, [`crate::VideoEncoder::gpu_target`])
    /// the frame is rendered into the encoder's input memory when possible (fork Phase 5.2c).
    pub fn frame_yuv(
        &self,
        f: i64,
        sources: &dyn SourceProvider,
        kr: f32,
        kb: f32,
        ten_bit: bool,
        chroma_422: bool,
        target: Option<&(dyn std::any::Any + Send + Sync)>,
    ) -> Option<GpuOut> {
        let gpu = self.gpu()?;
        let fx = &self.effects;
        if self.hdr_out || self.overlay.is_some() || fx.name_overlay.enabled || fx.timecode_overlay.enabled || fx.video_limiter.enabled {
            return None;
        }
        if self.geom.content.is_some_and(|c| c != (self.w as usize, self.h as usize)) || self.geom.offset != (0, 0) {
            return None;
        }
        let t = crate::trace::start();
        let plan = filmcraft_render::plan::plan_frame(&self.project, self.seq, self.rate.tick_of(f), self.opts, sources);
        crate::trace::add(crate::trace::PLAN, t);
        let filmcraft_render::plan::FramePlan::Layers { width, height, .. } = &plan else { return None };
        if (*width, *height) != (self.w as usize, self.h as usize) {
            return None;
        }
        if chroma_422 {
            return gpu.yuv422(&plan, kr, kb, ten_bit, self.opts.high_quality).map(GpuOut::Bytes);
        }
        if let Some(p) = target.and_then(|t| gpu.render_into(&plan, kr, kb, ten_bit, self.opts.high_quality, t)) {
            return Some(GpuOut::Picture(p));
        }
        gpu.yuv420(&plan, kr, kb, ten_bit, self.opts.high_quality).map(GpuOut::Bytes)
    }

    /// Fit the rendered picture into the output frame.
    fn place(&self, img: Image) -> Image {
        let (w, h) = (self.w as usize, self.h as usize);
        let img = match self.geom.content {
            Some((cw, ch)) if (cw, ch) != (img.w, img.h) => resample(&img, cw, ch),
            _ => img,
        };
        if img.w == w && img.h == h && self.geom.offset == (0, 0) {
            return img;
        }
        let mut out = Image::new(w, h);
        let (ox, oy) = self.geom.offset;
        let x0 = ox.max(0) as usize;
        let x1 = (ox + img.w as i64).clamp(0, w as i64) as usize;
        if x1 <= x0 {
            return out;
        }
        out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
            let sy = y as i64 - oy;
            if sy < 0 || sy >= img.h as i64 {
                return;
            }
            let sx0 = (x0 as i64 - ox) as usize;
            let src = &img.px[(sy as usize * img.w + sx0) * 4..(sy as usize * img.w + sx0 + (x1 - x0)) * 4];
            row[x0 * 4..x1 * 4].copy_from_slice(src);
        });
        out
    }

    fn overlays(&self, img: &mut Image, t: filmcraft_time::Tick) {
        let fx = &self.effects;
        if let Some(ov) = &self.overlay {
            let o = &fx.image_overlay;
            let (x, y) = anchor(o.placement, o.offset, img.w as f32, img.h as f32, ov.w as f32, ov.h as f32);
            composite(img, ov, x.round() as i64, y.round() as i64, o.opacity / 100.0);
        }
        if fx.name_overlay.enabled && !fx.name_overlay.text.trim().is_empty() {
            self.text(img, &fx.name_overlay, fx.name_overlay.text.trim());
        }
        if fx.timecode_overlay.enabled {
            let frame = self.seq_rate.frame_at(t) + self.start_tc;
            let tc = filmcraft_time::format_timecode_frames(frame, self.seq_rate, self.drop_frame);
            let text = if fx.timecode_overlay.text.trim().is_empty() { tc } else { format!("{} {tc}", fx.timecode_overlay.text.trim()) };
            self.text(img, &fx.timecode_overlay, &text);
        }
    }

    /// White text on a translucent black box (the Timecode / Clip Name burn-in look).
    fn text(&self, img: &mut Image, o: &TextOverlay, text: &str) {
        let px = (img.h as f32 * o.size_percent.clamp(1.0, 50.0) / 100.0).max(6.0);
        let family = filmcraft_text::fonts::DEFAULT_FAMILY;
        let st = filmcraft_text::TextStyle { family: family.into(), style: "Regular".into(), size: px, ..Default::default() };
        let l = filmcraft_text::layout(text, &st, &filmcraft_text::ParagraphStyle::default());
        let pad = px * 0.25;
        let (bw, bh) = (l.bounds[2] - l.bounds[0] + 2.0 * pad, px * 1.1 + 2.0 * pad);
        let (x, y) = anchor(o.placement, o.offset, img.w as f32, img.h as f32, bw, bh);
        let centre = Vec2::new((x + bw / 2.0) as f64, (y + bh / 2.0) as f64);
        let op = (o.opacity / 100.0).clamp(0.0, 1.0);
        if op >= 0.999 {
            filmcraft_render::graphics::burn_text(img, text, family, centre, px, 0.6);
            return;
        }
        // draw on a transparent layer, then composite it at the overlay's opacity
        let mut layer = Image::new(img.w, img.h);
        filmcraft_render::graphics::burn_text(&mut layer, text, family, centre, px, 0.6);
        composite(img, &layer, 0, 0, op);
    }
}

/// Top-left of a `bw`×`bh` box placed in a `w`×`h` frame (3 % margin) plus `offset`.
fn anchor(p: Placement, offset: (f32, f32), w: f32, h: f32, bw: f32, bh: f32) -> (f32, f32) {
    let (ax, ay) = p.anchor();
    let m = 0.03 * w.min(h);
    (m + ax * (w - 2.0 * m - bw) + offset.0, m + ay * (h - 2.0 * m - bh) + offset.1)
}

/// `src` (premultiplied) over `dst` at (`x`, `y`) with `opacity`.
fn composite(dst: &mut Image, src: &Image, x: i64, y: i64, opacity: f32) {
    let op = opacity.clamp(0.0, 1.0);
    if op <= 0.0 {
        return;
    }
    let w = dst.w;
    let xs = x.max(0)..(x + src.w as i64).min(w as i64);
    if xs.is_empty() {
        return;
    }
    dst.px.par_chunks_mut(w * 4).enumerate().for_each(|(dy, row)| {
        let sy = dy as i64 - y;
        if sy < 0 || sy >= src.h as i64 {
            return;
        }
        for dx in xs.clone() {
            let s = &src.px[(sy as usize * src.w + (dx - x) as usize) * 4..][..4];
            if s[3] <= 0.0 {
                continue;
            }
            let d = &mut row[dx as usize * 4..dx as usize * 4 + 4];
            let k = 1.0 - s[3] * op;
            for c in 0..4 {
                d[c] = s[c] * op + d[c] * k;
            }
        }
    });
}

/// Load a PNG / JPEG overlay as premultiplied linear light, `width` pixels wide.
fn load_overlay(path: &str, width: f32) -> Result<Image> {
    let bytes = std::fs::read(path).map_err(|e| ExportError::Io(format!("image overlay {path}: {e}")))?;
    let rgba = image::load_from_memory(&bytes).map_err(|e| ExportError::Unsupported(format!("image overlay {path}: {e}")))?.to_rgba8();
    let (iw, ih) = (rgba.width() as usize, rgba.height() as usize);
    let mut img = Image::new(iw, ih);
    for (o, p) in img.px.as_chunks_mut::<4>().0.iter_mut().zip(rgba.as_raw().as_chunks::<4>().0) {
        let a = p[3] as f32 / 255.0;
        for c in 0..3 {
            o[c] = filmcraft_color::srgb_to_linear(p[c] as f32 / 255.0) * a;
        }
        o[3] = a;
    }
    let nw = width.round().max(1.0) as usize;
    let nh = ((ih as f32 * nw as f32 / iw.max(1) as f32).round() as usize).max(1);
    Ok(if (nw, nh) == (iw, ih) { img } else { resample(&img, nw, nh) })
}

/// Separable resample of a premultiplied image with a triangle filter widened by the reduction
/// factor (area-correct when downscaling, bilinear when enlarging).
pub(crate) fn resample(src: &Image, nw: usize, nh: usize) -> Image {
    let tmp = resample_axis(src, nw, true);
    resample_axis(&tmp, nh, false)
}

fn weights(n_src: usize, n_dst: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = n_src as f32 / n_dst as f32;
    let support = scale.max(1.0);
    (0..n_dst)
        .map(|i| {
            let c = (i as f32 + 0.5) * scale - 0.5;
            let lo = ((c - support).ceil() as i64).max(0) as usize;
            let hi = ((c + support).floor() as i64).min(n_src as i64 - 1).max(lo as i64) as usize;
            let mut ws: Vec<f32> = (lo..=hi).map(|j| (1.0 - (j as f32 - c).abs() / support).max(0.0)).collect();
            let sum: f32 = ws.iter().sum();
            if sum > 0.0 {
                ws.iter_mut().for_each(|w| *w /= sum);
            } else {
                ws.iter_mut().for_each(|w| *w = 0.0);
                if let Some(w) = ws.first_mut() {
                    *w = 1.0;
                }
            }
            (lo, ws)
        })
        .collect()
}

fn resample_axis(src: &Image, n: usize, horizontal: bool) -> Image {
    let (w, h) = if horizontal { (n, src.h) } else { (src.w, n) };
    if (horizontal && n == src.w) || (!horizontal && n == src.h) {
        return src.clone();
    }
    let ws = weights(if horizontal { src.w } else { src.h }, n);
    let mut out = Image::new(w, h);
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let (lo, k) = if horizontal { &ws[x] } else { &ws[y] };
            let mut acc = [0f32; 4];
            for (j, wt) in k.iter().enumerate() {
                let (sx, sy) = if horizontal { (lo + j, y) } else { (x, lo + j) };
                let p = &src.px[(sy * src.w + sx) * 4..][..4];
                for c in 0..4 {
                    acc[c] += p[c] * wt;
                }
            }
            row[x * 4..x * 4 + 4].copy_from_slice(&acc);
        }
    });
    out
}

/// Keep BT.709 luma of display-encoded RGBA8 inside [min, max] percent: above the ceiling the
/// pixel is scaled down, below the floor it is lifted (hue kept), then channels are clipped.
pub fn limit_rgba8(rgba: &mut [u8], min_percent: f32, max_percent: f32) {
    let (lo, hi) = (min_percent.clamp(0.0, 100.0) / 100.0, max_percent.clamp(0.0, 100.0) / 100.0);
    rgba.par_chunks_mut(4 * 1024).for_each(|chunk| {
        for p in chunk.as_chunks_mut::<4>().0 {
            let mut c = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
            limit_px(&mut c, lo, hi);
            for i in 0..3 {
                p[i] = (c[i] * 255.0).floor().clamp(0.0, 255.0) as u8;
            }
        }
    });
}

fn limit_f32(rgb: &mut [f32], min_percent: f32, max_percent: f32) {
    let (lo, hi) = (min_percent.clamp(0.0, 100.0) / 100.0, max_percent.clamp(0.0, 100.0) / 100.0);
    rgb.par_chunks_mut(3 * 1024).for_each(|chunk| {
        for p in chunk.as_chunks_mut::<3>().0 {
            let mut c = [p[0], p[1], p[2]];
            limit_px(&mut c, lo, hi);
            p.copy_from_slice(&c);
        }
    });
}

fn limit_px(c: &mut [f32; 3], lo: f32, hi: f32) {
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    if y > hi && y > 0.0 {
        let k = hi / y;
        c.iter_mut().for_each(|v| *v *= k);
    } else if y < lo {
        let d = lo - y;
        c.iter_mut().for_each(|v| *v += d);
    }
    c.iter_mut().for_each(|v| *v = v.clamp(0.0, hi.max(lo)));
}

/// A frame rendered on the GPU ([`Pipeline::frame_yuv`]).
pub enum GpuOut {
    /// Planar 4:2:0 bytes read back.
    Bytes(Vec<u8>),
    /// In the encoder's input memory (fork Phase 5.2c).
    Picture(std::sync::Arc<dyn crate::GpuPicture>),
}

/// Temporal noise reduction (fork 6.7) at export: the export renders it, and its frame is the
/// Program monitor's paused frame (the frame server's options with temporal access).
#[cfg(test)]
mod temporal_tests {
    use super::*;
    use filmcraft_frame::{AudioBuffer, VideoFrame};
    use filmcraft_media::{FrameRequest, MediaInfo, MediaKind, MediaSource, VideoStreamInfo};
    use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Param, ParamValue, SequenceSettings, TrackKind};
    use filmcraft_render::SourceMap;
    use filmcraft_time::{Tick, TimeRange};

    const W: u32 = 96;
    const H: u32 = 64;
    const FRAMES: i64 = 8;

    /// A textured bar moving 2 px a frame over a static texture, with per-frame noise.
    struct Moving {
        info: MediaInfo,
    }

    impl MediaSource for Moving {
        fn info(&self) -> &MediaInfo {
            &self.info
        }
        fn video_frame(&self, req: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            let f = FrameRate::FPS_24.frame_at(req.time).clamp(0, FRAMES - 1);
            let mut seed = 0x2545_f491_4f6c_dd1du64 ^ (f as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let mut px = Vec::with_capacity((W * H * 4) as usize);
            for y in 0..H as i64 {
                for x in 0..W as i64 {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let noise = (seed >> 56) as i64 % 21 - 10;
                    let base = 90 + ((x * 7 + y * 3) % 40);
                    let bar = (x - 20 - 2 * f).rem_euclid(W as i64) < 16;
                    let v = (if bar { base + 90 } else { base } + noise).clamp(0, 255) as u8;
                    px.extend_from_slice(&[v, v.saturating_sub(10), v / 2 + 40, 255]);
                }
            }
            Ok(Arc::new(VideoFrame::rgba8(W, H, px)))
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> filmcraft_media::Result<AudioBuffer> {
            Ok(AudioBuffer::silence(sample_rate, 2, frames))
        }
    }

    fn project(tnr: bool) -> (Arc<Project>, ItemId, SourceMap) {
        let r = FrameRate::FPS_24;
        let info = MediaInfo {
            name: "moving-export-tnr".into(),
            kind: MediaKind::Movie,
            duration: r.tick_of(FRAMES),
            video: Some(VideoStreamInfo {
                camera_color_space: None,
                vfr: None,
                width: W,
                height: H,
                frame_rate: r,
                par: (1, 1),
                codec: "test".into(),
                pixel_format: "RGBA 8-bit".into(),
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
        let mut p = Project::new("tnr");
        let clip = MediaClip {
            media: MediaRef::File { path: "moving.mov".into() },
            info: info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        let id = p.add_item("moving", Label::Iris, ItemKind::Media(clip), None);
        let seq = p.new_sequence("s", SequenceSettings { width: W, height: H, frame_rate: r, ..Default::default() }, 1, 0, None);
        let mut it = p.make_track_item(id, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(FRAMES)), r).unwrap();
        for e in &mut it.effects {
            filmcraft_project::resolve_auto_points(e, (W, H), (W, H));
        }
        if tnr {
            let mut n = filmcraft_project::TemporalNr::default();
            for (k, v) in [("luma", 1.0), ("chroma", 1.0), ("frames", 2.0)] {
                n.params.insert(k.into(), Param::new(ParamValue::Float(v)));
            }
            it.temporal_nr = Some(n);
        }
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(it);
        let mut m = SourceMap::default();
        m.0.insert(id, Arc::new(Moving { info }) as Arc<dyn MediaSource>);
        (Arc::new(p), seq, m)
    }

    #[test]
    fn export_renders_temporal_nr_like_the_paused_program_frame() {
        let settings = ExportSettings { gpu_rendering: GpuRendering::Off, ..Default::default() };
        let (p, seq, m) = project(true);
        let pipe = Pipeline::new(p.clone(), seq, &settings, false).unwrap();
        assert!(pipe.opts.temporal, "every export (and render-cache part) has temporal access");
        let f = 4;
        let (exported, _) = pipe.frame(f, &m);
        let t = pipe.rate.tick_of(f);
        // the frame server's options for the paused Program frame (`frames.rs`)
        let paused = RenderOptions { scale: 1.0, captions: true, temporal: true, ..Default::default() };
        let viewer = filmcraft_render::render_sequence(&p, seq, t, paused, &m).over_black_rgba8();
        assert_eq!(exported, viewer, "export frame == paused viewer frame");
        // ...and the GPU plan's CPU reference matches it (the GPU path: lane A's parity tests)
        let plan = filmcraft_render::plan::execute_cpu(&filmcraft_render::plan::plan_frame(&p, seq, t, paused, &m)).over_black_rgba8();
        let worst = plan.iter().zip(&viewer).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(worst <= 1, "plan vs CPU: {worst} code values");
        // while playing the clip shows without it, and an export without tnr is that picture
        let live = filmcraft_render::render_sequence(&p, seq, t, RenderOptions { temporal: false, ..paused }, &m).over_black_rgba8();
        assert_ne!(exported, live, "temporal NR changed the exported frame");
        let (q, qseq, qm) = project(false);
        let plain = Pipeline::new(q, qseq, &settings, false).unwrap().frame(f, &qm).0;
        assert_eq!(plain, live);
    }
}
