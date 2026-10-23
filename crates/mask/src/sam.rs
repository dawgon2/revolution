//! MobileSAM (Zhang et al. 2023, Apache-2.0: a TinyViT-5M image encoder distilled from SAM's
//! ViT-H, with SAM's prompt encoder and mask decoder) on the CPU via candle.
//!
//! Loads the official `mobile_sam.pt` checkpoint (a PyTorch state dict) with candle's checked
//! pickle reader. The frame is scaled so its longest side is 1024, normalised with SAM's pixel
//! mean / std and zero-padded to 1024 × 1024; the encoder gives a 256 × 64 × 64 embedding.
//! Prompts are points, a box and / or the previous frame's 256 × 256 logits.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use candle_core::{DType, Device, IndexOp, Module, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::segment_anything::mask_decoder::MaskDecoder;
use candle_transformers::models::segment_anything::prompt_encoder::PromptEncoder;
use candle_transformers::models::segment_anything::tiny_vit::{TinyViT, tiny_vit_5m};

use crate::{Embedding, LOW_RES, MaskError, Prompt, RgbImage, Segment, Segmenter};

/// The encoder's input side.
pub const IMAGE_SIZE: usize = 1024;
const EMBED_SIDE: usize = 64;
const PROMPT_EMBED_DIM: usize = 256;
/// The official checkpoint is 40.7 MB; refuse anything far outside that before parsing it.
const MIN_BYTES: u64 = 1 << 20;
const MAX_BYTES: u64 = 512 << 20;
const PIXEL_MEAN: [f32; 3] = [123.675, 116.28, 103.53];
const PIXEL_STD: [f32; 3] = [58.395, 57.12, 57.375];

fn failed(e: candle_core::Error) -> MaskError {
    MaskError::Failed(e.to_string())
}

/// Run model code under `catch_unwind` (candle may panic on shapes it doesn't expect).
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, MaskError>) -> Result<T, MaskError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => Err(MaskError::Failed(format!("{what} stopped unexpectedly ({})", filmcraft_ml::job::panic_message(p.as_ref())))),
    }
}

/// MobileSAM, loaded and ready.
pub struct MobileSam {
    encoder: TinyViT,
    prompt: PromptEncoder,
    decoder: MaskDecoder,
    image_pe: Tensor,
    /// The prompt encoder's random-Fourier matrix (2 × 128) and its box-corner embeddings
    /// (top-left, bottom-right; 1 × 256 each): candle 0.9's `embed_boxes` returns a rank-2
    /// tensor that can't be joined with points, so boxes are embedded here (as in SAM).
    pe_gauss: Tensor,
    corners: [Tensor; 2],
    device: Device,
    /// The checkpoint, re-read by [`MobileSam::encoder_weights`].
    path: std::path::PathBuf,
}

/// One of the image encoder's tensors as plain data (for an encoder outside candle, e.g. the
/// GPU one in `filmcraft-gpu`): its name without the `image_encoder.` prefix, its shape and its
/// values (row-major, f32).
#[derive(Clone, Debug, PartialEq)]
pub struct NamedTensor {
    pub name: String,
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

/// Values in one embedding: 256 channels × 64 × 64.
pub const EMBEDDING_LEN: usize = PROMPT_EMBED_DIM * EMBED_SIDE * EMBED_SIDE;
/// Most values the image encoder's tensors may hold together (TinyViT-5M has ~5.4 M).
const MAX_ENCODER_VALUES: usize = 32 << 20;

impl std::fmt::Debug for MobileSam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MobileSam")
    }
}

/// The encoder output for one frame, plus how the frame was scaled into the 1024 square.
struct SamEmbedding {
    features: Tensor,
    /// The scaled frame's size inside the 1024 square.
    fitted: (usize, usize),
}

impl MobileSam {
    /// Load `mobile_sam.pt`. A missing file is [`MaskError::ModelMissing`]; a file of the wrong
    /// size, an unreadable pickle or missing / misshapen weights are [`MaskError::ModelCorrupt`].
    pub fn load(path: &Path) -> Result<Self, MaskError> {
        let meta = std::fs::metadata(path).map_err(|_| MaskError::ModelMissing(path.display().to_string()))?;
        if !meta.is_file() {
            return Err(MaskError::ModelMissing(path.display().to_string()));
        }
        if meta.len() < MIN_BYTES || meta.len() > MAX_BYTES {
            return Err(MaskError::ModelCorrupt(format!("{} bytes, expected about 40 MB", meta.len())));
        }
        let corrupt = |e: candle_core::Error| MaskError::ModelCorrupt(e.to_string());
        let device = Device::Cpu;
        match catch_unwind(AssertUnwindSafe(|| -> Result<Self, MaskError> {
            let vb = VarBuilder::from_pth(path, DType::F32, &device).map_err(corrupt)?;
            let encoder = tiny_vit_5m(vb.pp("image_encoder")).map_err(corrupt)?;
            let prompt =
                PromptEncoder::new(PROMPT_EMBED_DIM, (EMBED_SIDE, EMBED_SIDE), (IMAGE_SIZE, IMAGE_SIZE), 16, vb.pp("prompt_encoder")).map_err(corrupt)?;
            let decoder = MaskDecoder::new(PROMPT_EMBED_DIM, 3, 3, 256, vb.pp("mask_decoder")).map_err(corrupt)?;
            let image_pe = prompt.get_dense_pe().map_err(corrupt)?;
            let pe = vb.pp("prompt_encoder");
            let pe_gauss = pe.get((2, PROMPT_EMBED_DIM / 2), "pe_layer.positional_encoding_gaussian_matrix").map_err(corrupt)?;
            let corner = |i: usize| pe.get((1, PROMPT_EMBED_DIM), &format!("point_embeddings.{i}.weight")).map_err(corrupt);
            let corners = [corner(2)?, corner(3)?];
            Ok(Self { encoder, prompt, decoder, image_pe, pe_gauss, corners, device: device.clone(), path: path.to_path_buf() })
        })) {
            Ok(r) => r,
            Err(p) => Err(MaskError::ModelCorrupt(filmcraft_ml::job::panic_message(p.as_ref()))),
        }
    }

    /// The image encoder's tensors as plain data (names without `image_encoder.`), read again
    /// from the checkpoint. A damaged file is [`MaskError::ModelCorrupt`].
    pub fn encoder_weights(&self) -> Result<Vec<NamedTensor>, MaskError> {
        let corrupt = |e: candle_core::Error| MaskError::ModelCorrupt(e.to_string());
        match catch_unwind(AssertUnwindSafe(|| -> Result<Vec<NamedTensor>, MaskError> {
            let pth = candle_core::pickle::PthTensors::new(&self.path, None).map_err(corrupt)?;
            let mut names: Vec<String> = pth.tensor_infos().keys().filter(|n| n.starts_with("image_encoder.")).cloned().collect();
            names.sort();
            let mut out = Vec::with_capacity(names.len());
            let mut total = 0usize;
            for name in names {
                let Some(t) = pth.get(&name).map_err(corrupt)? else { continue };
                let shape = t.dims().to_vec();
                total = total.saturating_add(t.elem_count());
                if total > MAX_ENCODER_VALUES {
                    return Err(MaskError::ModelCorrupt(format!("the image encoder holds over {MAX_ENCODER_VALUES} values")));
                }
                let data: Vec<f32> = t.to_dtype(DType::F32).and_then(|t| t.flatten_all()).and_then(|t| t.to_vec1()).map_err(corrupt)?;
                let short = name.trim_start_matches("image_encoder.").to_string();
                out.push(NamedTensor { name: short, shape, data });
            }
            if out.is_empty() {
                return Err(MaskError::ModelCorrupt("no image encoder tensors".into()));
            }
            Ok(out)
        })) {
            Ok(r) => r,
            Err(p) => Err(MaskError::ModelCorrupt(filmcraft_ml::job::panic_message(p.as_ref()))),
        }
    }

    /// The encoder's input for `image` as plain data: (1, 3, 1024, 1024) normalised f32
    /// (see [`MobileSam::input`]) and the scaled frame's size inside the 1024 square.
    pub fn prepare_input(&self, image: &RgbImage) -> Result<(Vec<f32>, (usize, usize)), MaskError> {
        input_data(image)
    }

    /// An [`Embedding`] from encoder output made elsewhere (the GPU encoder): `features` is the
    /// (1, 256, 64, 64) embedding, `fitted` what [`MobileSam::prepare_input`] returned for the
    /// `image_w` × `image_h` frame. Wrong lengths, sizes or non-finite values are refused.
    pub fn embedding_from_features(&self, features: Vec<f32>, image_w: usize, image_h: usize, fitted: (usize, usize)) -> Result<Embedding, MaskError> {
        crate::check_size(image_w, image_h)?;
        if features.len() != EMBEDDING_LEN {
            return Err(MaskError::Failed(format!("an embedding of {} values, expected {EMBEDDING_LEN}", features.len())));
        }
        if features.iter().any(|v| !v.is_finite()) {
            return Err(MaskError::Failed("the embedding has non-finite values".into()));
        }
        let ok = |v: usize| (1..=IMAGE_SIZE).contains(&v);
        if !ok(fitted.0) || !ok(fitted.1) {
            return Err(MaskError::BadImage(format!("fitted size {}×{}", fitted.0, fitted.1)));
        }
        let features = Tensor::from_vec(features, (1, PROMPT_EMBED_DIM, EMBED_SIDE, EMBED_SIDE), &self.device).map_err(failed)?;
        Ok(Embedding::new(image_w, image_h, Box::new(SamEmbedding { features, fitted })))
    }

    /// The CPU encoder's output for a prepared input (1, 3, 1024, 1024), as plain data.
    pub fn encode_cpu(&self, input: &[f32]) -> Result<Vec<f32>, MaskError> {
        let t = input_tensor(input, &self.device)?;
        guarded("the image encoder", || self.encoder.forward(&t).and_then(|t| t.flatten_all()).and_then(|t| t.to_vec1()).map_err(failed))
    }

    /// The CPU encoder's intermediate outputs (parity tests of other encoders): see
    /// [`crate::tinyvit_ref`]. Reads the checkpoint again; slow.
    #[doc(hidden)]
    pub fn encoder_stages_cpu(&self, input: &[f32]) -> Result<Vec<(String, Vec<f32>)>, MaskError> {
        let t = input_tensor(input, &self.device)?;
        guarded("the reference encoder", || crate::tinyvit_ref::stages(&self.path, &t, &self.device))
    }

    /// The frame scaled so its longest side is 1024 (an area average when shrinking, so a 6.2K
    /// frame doesn't alias; bilinear when growing), normalised, zero-padded: a
    /// (1, 3, 1024, 1024) tensor, and the scaled size.
    fn input(&self, image: &RgbImage) -> Result<(Tensor, (usize, usize)), MaskError> {
        let (buf, fitted) = input_data(image)?;
        Ok((input_tensor(&buf, &self.device)?, fitted))
    }

    /// SAM's box embedding: the two corners (pixel centres, in the 1024 square) through the
    /// random-Fourier encoding plus the corner embeddings: (1, 2, 256).
    fn embed_box(&self, b: [f32; 4]) -> candle_core::Result<Tensor> {
        let s = IMAGE_SIZE as f32;
        let coords: Vec<f32> = b.iter().map(|c| (c + 0.5) / s * 2.0 - 1.0).collect();
        let coords = Tensor::from_vec(coords, (2, 2), &self.device)?;
        let proj = (coords.matmul(&self.pe_gauss)? * (2.0 * std::f64::consts::PI))?;
        let enc = Tensor::cat(&[proj.sin()?, proj.cos()?], 1)?;
        let c0 = (enc.i(0..1)? + &self.corners[0])?;
        let c1 = (enc.i(1..2)? + &self.corners[1])?;
        Tensor::cat(&[c0, c1], 0)?.unsqueeze(0)
    }
}

/// See [`MobileSam::prepare_input`].
fn input_data(image: &RgbImage) -> Result<(Vec<f32>, (usize, usize)), MaskError> {
    let (w, h) = (image.width(), image.height());
    let scale = IMAGE_SIZE as f32 / w.max(h) as f32;
    let fw = ((w as f32 * scale).round() as usize).clamp(1, IMAGE_SIZE);
    let fh = ((h as f32 * scale).round() as usize).clamp(1, IMAGE_SIZE);
    let plane = IMAGE_SIZE * IMAGE_SIZE;
    let mut buf = vec![0f32; 3 * plane];
    let data = image.data();
    let at = |x: usize, y: usize, c: usize| data.get((y * w + x) * 3 + c).copied().unwrap_or(0) as f32;
    let (sx, sy) = (w as f32 / fw as f32, h as f32 / fh as f32);
    // Source span [a, b) of output pixel `i` when shrinking (at least one pixel).
    let span = |i: usize, s: f32, n: usize| {
        let a = ((i as f32 * s) as usize).min(n - 1);
        let b = (((i + 1) as f32 * s).ceil() as usize).clamp(a + 1, n);
        (a, b)
    };
    for y in 0..fh {
        let syf = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (h - 1) as f32);
        let y0 = syf.floor() as usize;
        let y1 = (y0 + 1).min(h - 1);
        let fy = syf - y0 as f32;
        for x in 0..fw {
            let sxf = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (w - 1) as f32);
            let x0 = sxf.floor() as usize;
            let x1 = (x0 + 1).min(w - 1);
            let fx = sxf - x0 as f32;
            for c in 0..3 {
                let v = if sx > 1.0 || sy > 1.0 {
                    let ((xa, xb), (ya, yb)) = (span(x, sx, w), span(y, sy, h));
                    let mut sum = 0.0;
                    for yy in ya..yb {
                        for xx in xa..xb {
                            sum += at(xx, yy, c);
                        }
                    }
                    sum / ((xb - xa) * (yb - ya)).max(1) as f32
                } else {
                    let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
                    let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
                    top * (1.0 - fy) + bottom * fy
                };
                if let Some(slot) = buf.get_mut(c * plane + y * IMAGE_SIZE + x) {
                    *slot = (v - PIXEL_MEAN[c]) / PIXEL_STD[c];
                }
            }
        }
    }
    Ok((buf, (fw, fh)))
}

fn input_tensor(input: &[f32], device: &Device) -> Result<Tensor, MaskError> {
    if input.len() != 3 * IMAGE_SIZE * IMAGE_SIZE {
        return Err(MaskError::BadImage(format!("an encoder input of {} values, expected {}", input.len(), 3 * IMAGE_SIZE * IMAGE_SIZE)));
    }
    Tensor::from_vec(input.to_vec(), (1, 3, IMAGE_SIZE, IMAGE_SIZE), device).map_err(failed)
}

#[cfg(test)]
impl MobileSam {
    /// candle's own box embedding (rank 2: (1, 512)), to check [`MobileSam::embed_box`] against.
    pub(crate) fn candle_box_and_ours(&self, b: [f32; 4]) -> candle_core::Result<(Vec<f32>, Vec<f32>)> {
        let t = Tensor::from_vec(b.to_vec(), (1, 4), &self.device)?;
        let (theirs, _) = self.prompt.forward(None, Some(&t), None)?;
        Ok((theirs.flatten_all()?.to_vec1()?, self.embed_box(b)?.flatten_all()?.to_vec1()?))
    }
}

/// Diagnostics (the acceptance harness caches encoder outputs on disk): an embedding's raw
/// features and fitted size, and back. Not a stable interface.
#[doc(hidden)]
impl MobileSam {
    pub fn export_embedding(&self, e: &Embedding) -> Result<(Vec<f32>, (usize, usize)), MaskError> {
        let emb: &SamEmbedding = e.inner()?;
        let v = emb.features.flatten_all().and_then(|t| t.to_vec1()).map_err(failed)?;
        Ok((v, emb.fitted))
    }
    pub fn import_embedding(&self, size: (usize, usize), features: Vec<f32>, fitted: (usize, usize)) -> Result<Embedding, MaskError> {
        if features.len() != PROMPT_EMBED_DIM * EMBED_SIDE * EMBED_SIDE {
            return Err(MaskError::BadImage(format!("{} embedding values", features.len())));
        }
        let features = Tensor::from_vec(features, (1, PROMPT_EMBED_DIM, EMBED_SIDE, EMBED_SIDE), &self.device).map_err(failed)?;
        Ok(Embedding::new(size.0, size.1, Box::new(SamEmbedding { features, fitted })))
    }
}

impl Segmenter for MobileSam {
    fn embed(&self, image: &RgbImage) -> Result<Embedding, MaskError> {
        let (input, fitted) = self.input(image)?;
        let features = guarded("the image encoder", || self.encoder.forward(&input).map_err(failed))?;
        Ok(Embedding::new(image.width(), image.height(), Box::new(SamEmbedding { features, fitted })))
    }

    fn segment(&self, embedding: &Embedding, prompt: &Prompt) -> Result<Segment, MaskError> {
        // One clicked point is ambiguous (shirt / person / group): ask for SAM's three
        // candidates and keep the best-scored; otherwise one mask.
        let (points, rect) = prompt.checked()?;
        let multimask = points.len() == 1 && rect.is_none() && prompt.prior.is_none();
        let all = self.decode(embedding, prompt, multimask)?;
        all.into_iter()
            .fold(None, |b: Option<Segment>, s| match b {
                Some(b) if b.score >= s.score => Some(b),
                _ => Some(s),
            })
            .ok_or_else(|| MaskError::Failed("the decoder gave no mask".into()))
    }

    fn candidates(&self, embedding: &Embedding, prompt: &Prompt, multimask: bool) -> Result<Vec<Segment>, MaskError> {
        self.decode(embedding, prompt, multimask)
    }
}

impl MobileSam {
    /// The decoder's candidate masks for a prompt: SAM's three (whole / part / sub-part) when
    /// `multimask`, else its single mask; each with the model's predicted IoU as `score`.
    fn decode(&self, embedding: &Embedding, prompt: &Prompt, multimask: bool) -> Result<Vec<Segment>, MaskError> {
        let emb: &SamEmbedding = embedding.inner()?;
        let (points, rect) = prompt.checked()?;
        let (fw, fh) = (emb.fitted.0 as f32, emb.fitted.1 as f32);
        let dev = &self.device;
        guarded("the mask decoder", || {
            let pts = if points.is_empty() {
                None
            } else {
                let xy: Vec<f32> = points.iter().flat_map(|p| [p.x * fw, p.y * fh]).collect();
                let labels: Vec<f32> = points.iter().map(|p| if p.positive { 1.0 } else { 0.0 }).collect();
                let n = points.len();
                Some((Tensor::from_vec(xy, (1, n, 2), dev).map_err(failed)?, Tensor::from_vec(labels, (1, n), dev).map_err(failed)?))
            };
            let boxes = match rect {
                Some(r) => Some(self.embed_box([r.x0 * fw, r.y0 * fh, r.x1 * fw, r.y1 * fh]).map_err(failed)?),
                None => None,
            };
            let cover = (fw / IMAGE_SIZE as f32, fh / IMAGE_SIZE as f32);
            let prior = match &prompt.prior {
                Some(s) if (s.cover.0 - cover.0).abs() > 1.0 / LOW_RES as f32 || (s.cover.1 - cover.1).abs() > 1.0 / LOW_RES as f32 => {
                    return Err(MaskError::BadPrompt("the previous mask was made on a frame of another shape".into()));
                }
                Some(s) if s.logits.len() == LOW_RES * LOW_RES => {
                    let v: Vec<f32> = s.logits.iter().map(|l| if l.is_finite() { l.clamp(-50.0, 50.0) } else { 0.0 }).collect();
                    Some(Tensor::from_vec(v, (1, 1, LOW_RES, LOW_RES), dev).map_err(failed)?)
                }
                Some(_) => return Err(MaskError::BadPrompt("previous mask has the wrong size".into())),
                None => None,
            };
            let (sparse, dense) = self.prompt.forward(pts.as_ref().map(|(a, b)| (a, b)), None, prior.as_ref()).map_err(failed)?;
            // Without a box candle pads the points with one "not a point" token at the end; SAM
            // doesn't when there is a box, so drop it and append the box corners.
            let sparse = match boxes {
                Some(b) => {
                    let pts = sparse.narrow(1, 0, points.len()).map_err(failed)?;
                    Tensor::cat(&[pts, b], 1).map_err(failed)?
                }
                None => sparse,
            };
            let (masks, iou) = self.decoder.forward(&emb.features, &self.image_pe, &sparse, &dense, multimask).map_err(failed)?;
            let scores: Vec<f32> = iou.flatten_all().and_then(|t| t.to_vec1()).map_err(failed)?;
            let mut out = Vec::with_capacity(scores.len());
            for (i, score) in scores.iter().enumerate() {
                let logits: Vec<f32> = masks.i((0, i)).and_then(|t| t.flatten_all()).and_then(|t| t.to_vec1()).map_err(failed)?;
                if logits.len() != LOW_RES * LOW_RES {
                    return Err(MaskError::Failed(format!("the decoder gave {} values, expected {}", logits.len(), LOW_RES * LOW_RES)));
                }
                out.push(Segment { logits, cover, score: if score.is_finite() { score.clamp(0.0, 1.0) } else { 0.0 } });
            }
            Ok(out)
        })
    }
}
