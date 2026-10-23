//! Loading an official SAM 2.1 checkpoint (`sam2.1_hiera_*.pt`, a PyTorch zip whose `model`
//! entry is the state dict) with candle's checked pickle reader, against the tensor list its
//! config implies ([`expected`]). Anything missing, misshapen or non-finite is refused with
//! [`MaskError::ModelCorrupt`] in plain words; a missing file is [`MaskError::ModelMissing`].

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use candle_core::{DType, Device, Tensor};

use super::config::{D_MODEL, MEM_DIM, Sam2Config};
use crate::MaskError;

/// The official checkpoints are 324 MB (Base+) and 898 MB (Large); refuse anything far outside.
const MIN_BYTES: u64 = 16 << 20;
const MAX_BYTES: u64 = 4 << 30;
/// Most values all tensors together may hold (Large has ~224 M).
const MAX_VALUES: usize = 600 << 20;

/// One tensor as plain data (row-major f32), for a device outside candle.
#[derive(Clone, Debug, PartialEq)]
pub struct PlainTensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

/// A loaded, checked SAM 2.1 checkpoint.
pub struct Sam2Weights {
    pub cfg: Sam2Config,
    tensors: HashMap<String, Tensor>,
}

impl std::fmt::Debug for Sam2Weights {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sam2Weights({} tensors)", self.tensors.len())
    }
}

fn corrupt(e: impl std::fmt::Display) -> MaskError {
    MaskError::ModelCorrupt(e.to_string())
}

/// Every tensor the port uses, with its shape, for this config.
pub fn expected(cfg: &Sam2Config) -> Vec<(String, Vec<usize>)> {
    let mut v: Vec<(String, Vec<usize>)> = Vec::new();
    let mut add = |n: String, s: &[usize]| v.push((n, s.to_vec()));
    let c = cfg.embed_dim;
    let t = "image_encoder.trunk";
    add(format!("{t}.pos_embed"), &[1, c, cfg.pos_bkg.0, cfg.pos_bkg.1]);
    add(format!("{t}.pos_embed_window"), &[1, c, cfg.window_spec[0], cfg.window_spec[0]]);
    add(format!("{t}.patch_embed.proj.weight"), &[c, 3, 7, 7]);
    add(format!("{t}.patch_embed.proj.bias"), &[c]);
    for (i, b) in cfg.blocks().iter().enumerate() {
        let p = format!("{t}.blocks.{i}");
        let (d, o) = (b.dim, b.dim_out);
        for (n, s) in [
            ("norm1.weight", vec![d]),
            ("norm1.bias", vec![d]),
            ("attn.qkv.weight", vec![3 * o, d]),
            ("attn.qkv.bias", vec![3 * o]),
            ("attn.proj.weight", vec![o, o]),
            ("attn.proj.bias", vec![o]),
            ("norm2.weight", vec![o]),
            ("norm2.bias", vec![o]),
            ("mlp.layers.0.weight", vec![4 * o, o]),
            ("mlp.layers.0.bias", vec![4 * o]),
            ("mlp.layers.1.weight", vec![o, 4 * o]),
            ("mlp.layers.1.bias", vec![o]),
        ] {
            add(format!("{p}.{n}"), &s);
        }
        if d != o {
            add(format!("{p}.proj.weight"), &[o, d]);
            add(format!("{p}.proj.bias"), &[o]);
        }
    }
    for (k, ch) in cfg.backbone_channels.iter().enumerate() {
        add(format!("image_encoder.neck.convs.{k}.conv.weight"), &[D_MODEL, *ch, 1, 1]);
        add(format!("image_encoder.neck.convs.{k}.conv.bias"), &[D_MODEL]);
    }
    // memory attention
    let lin = |add: &mut dyn FnMut(String, &[usize]), p: &str, o: usize, i: usize| {
        add(format!("{p}.weight"), &[o, i]);
        add(format!("{p}.bias"), &[o]);
    };
    let ln = |add: &mut dyn FnMut(String, &[usize]), p: &str, c: usize| {
        add(format!("{p}.weight"), &[c]);
        add(format!("{p}.bias"), &[c]);
    };
    for l in 0..cfg.mem_layers {
        let p = format!("memory_attention.layers.{l}");
        for a in ["q", "k", "v", "out"] {
            lin(&mut add, &format!("{p}.self_attn.{a}_proj"), D_MODEL, D_MODEL);
        }
        lin(&mut add, &format!("{p}.cross_attn_image.q_proj"), D_MODEL, D_MODEL);
        lin(&mut add, &format!("{p}.cross_attn_image.k_proj"), D_MODEL, MEM_DIM);
        lin(&mut add, &format!("{p}.cross_attn_image.v_proj"), D_MODEL, MEM_DIM);
        lin(&mut add, &format!("{p}.cross_attn_image.out_proj"), D_MODEL, D_MODEL);
        lin(&mut add, &format!("{p}.linear1"), cfg.mem_ffn, D_MODEL);
        lin(&mut add, &format!("{p}.linear2"), D_MODEL, cfg.mem_ffn);
        for n in ["norm1", "norm2", "norm3"] {
            ln(&mut add, &format!("{p}.{n}"), D_MODEL);
        }
    }
    ln(&mut add, "memory_attention.norm", D_MODEL);
    // memory encoder
    let m = "memory_encoder";
    let chans = [1usize, 4, 16, 64, 256];
    for i in 0..4 {
        let (ci, co) = (chans[i], chans[i + 1]);
        add(format!("{m}.mask_downsampler.encoder.{}.weight", 3 * i), &[co, ci, 3, 3]);
        add(format!("{m}.mask_downsampler.encoder.{}.bias", 3 * i), &[co]);
        ln(&mut add, &format!("{m}.mask_downsampler.encoder.{}", 3 * i + 1), co);
    }
    add(format!("{m}.mask_downsampler.encoder.12.weight"), &[D_MODEL, D_MODEL, 1, 1]);
    add(format!("{m}.mask_downsampler.encoder.12.bias"), &[D_MODEL]);
    add(format!("{m}.pix_feat_proj.weight"), &[D_MODEL, D_MODEL, 1, 1]);
    add(format!("{m}.pix_feat_proj.bias"), &[D_MODEL]);
    for l in 0..cfg.fuser_layers {
        let p = format!("{m}.fuser.layers.{l}");
        add(format!("{p}.gamma"), &[D_MODEL]);
        add(format!("{p}.dwconv.weight"), &[D_MODEL, 1, 7, 7]);
        add(format!("{p}.dwconv.bias"), &[D_MODEL]);
        ln(&mut add, &format!("{p}.norm"), D_MODEL);
        lin(&mut add, &format!("{p}.pwconv1"), 4 * D_MODEL, D_MODEL);
        lin(&mut add, &format!("{p}.pwconv2"), D_MODEL, 4 * D_MODEL);
    }
    add(format!("{m}.out_proj.weight"), &[MEM_DIM, D_MODEL, 1, 1]);
    add(format!("{m}.out_proj.bias"), &[MEM_DIM]);
    // SAM2Base parameters
    add("maskmem_tpos_enc".into(), &[cfg.num_maskmem, 1, 1, MEM_DIM]);
    add("no_mem_embed".into(), &[1, 1, D_MODEL]);
    add("no_obj_ptr".into(), &[1, D_MODEL]);
    add("no_obj_embed_spatial".into(), &[1, MEM_DIM]);
    for l in 0..3 {
        lin(&mut add, &format!("obj_ptr_proj.layers.{l}"), D_MODEL, D_MODEL);
    }
    lin(&mut add, "obj_ptr_tpos_proj", MEM_DIM, D_MODEL);
    // prompt encoder
    let pe = "sam_prompt_encoder";
    add(format!("{pe}.pe_layer.positional_encoding_gaussian_matrix"), &[2, D_MODEL / 2]);
    for i in 0..4 {
        add(format!("{pe}.point_embeddings.{i}.weight"), &[1, D_MODEL]);
    }
    add(format!("{pe}.not_a_point_embed.weight"), &[1, D_MODEL]);
    add(format!("{pe}.no_mask_embed.weight"), &[1, D_MODEL]);
    add(format!("{pe}.mask_downscaling.0.weight"), &[4, 1, 2, 2]);
    add(format!("{pe}.mask_downscaling.0.bias"), &[4]);
    ln(&mut add, &format!("{pe}.mask_downscaling.1"), 4);
    add(format!("{pe}.mask_downscaling.3.weight"), &[16, 4, 2, 2]);
    add(format!("{pe}.mask_downscaling.3.bias"), &[16]);
    ln(&mut add, &format!("{pe}.mask_downscaling.4"), 16);
    add(format!("{pe}.mask_downscaling.6.weight"), &[D_MODEL, 16, 1, 1]);
    add(format!("{pe}.mask_downscaling.6.bias"), &[D_MODEL]);
    // mask decoder
    let d = "sam_mask_decoder";
    let half = D_MODEL / 2;
    for l in 0..2 {
        let p = format!("{d}.transformer.layers.{l}");
        for a in ["q", "k", "v", "out"] {
            lin(&mut add, &format!("{p}.self_attn.{a}_proj"), D_MODEL, D_MODEL);
        }
        for x in ["cross_attn_token_to_image", "cross_attn_image_to_token"] {
            for a in ["q", "k", "v"] {
                lin(&mut add, &format!("{p}.{x}.{a}_proj"), half, D_MODEL);
            }
            lin(&mut add, &format!("{p}.{x}.out_proj"), D_MODEL, half);
        }
        lin(&mut add, &format!("{p}.mlp.layers.0"), 2048, D_MODEL);
        lin(&mut add, &format!("{p}.mlp.layers.1"), D_MODEL, 2048);
        for n in ["norm1", "norm2", "norm3", "norm4"] {
            ln(&mut add, &format!("{p}.{n}"), D_MODEL);
        }
    }
    for a in ["q", "k", "v"] {
        lin(&mut add, &format!("{d}.transformer.final_attn_token_to_image.{a}_proj"), half, D_MODEL);
    }
    lin(&mut add, &format!("{d}.transformer.final_attn_token_to_image.out_proj"), D_MODEL, half);
    ln(&mut add, &format!("{d}.transformer.norm_final_attn"), D_MODEL);
    add(format!("{d}.iou_token.weight"), &[1, D_MODEL]);
    add(format!("{d}.mask_tokens.weight"), &[4, D_MODEL]);
    add(format!("{d}.obj_score_token.weight"), &[1, D_MODEL]);
    add(format!("{d}.output_upscaling.0.weight"), &[D_MODEL, D_MODEL / 4, 2, 2]);
    add(format!("{d}.output_upscaling.0.bias"), &[D_MODEL / 4]);
    ln(&mut add, &format!("{d}.output_upscaling.1"), D_MODEL / 4);
    add(format!("{d}.output_upscaling.3.weight"), &[D_MODEL / 4, D_MODEL / 8, 2, 2]);
    add(format!("{d}.output_upscaling.3.bias"), &[D_MODEL / 8]);
    add(format!("{d}.conv_s0.weight"), &[D_MODEL / 8, D_MODEL, 1, 1]);
    add(format!("{d}.conv_s0.bias"), &[D_MODEL / 8]);
    add(format!("{d}.conv_s1.weight"), &[D_MODEL / 4, D_MODEL, 1, 1]);
    add(format!("{d}.conv_s1.bias"), &[D_MODEL / 4]);
    for i in 0..4 {
        lin(&mut add, &format!("{d}.output_hypernetworks_mlps.{i}.layers.0"), D_MODEL, D_MODEL);
        lin(&mut add, &format!("{d}.output_hypernetworks_mlps.{i}.layers.1"), D_MODEL, D_MODEL);
        lin(&mut add, &format!("{d}.output_hypernetworks_mlps.{i}.layers.2"), D_MODEL / 8, D_MODEL);
    }
    for (h, out) in [("iou_prediction_head", 4), ("pred_obj_score_head", 1)] {
        lin(&mut add, &format!("{d}.{h}.layers.0"), D_MODEL, D_MODEL);
        lin(&mut add, &format!("{d}.{h}.layers.1"), D_MODEL, D_MODEL);
        lin(&mut add, &format!("{d}.{h}.layers.2"), out, D_MODEL);
    }
    v
}

impl Sam2Weights {
    /// Load `checkpoint` for `cfg`: every tensor [`expected`] lists must be there with that
    /// shape and finite values.
    pub fn load(checkpoint: &Path, cfg: Sam2Config) -> Result<Self, MaskError> {
        cfg.validate()?;
        let meta = std::fs::metadata(checkpoint).map_err(|_| MaskError::ModelMissing(checkpoint.display().to_string()))?;
        if !meta.is_file() {
            return Err(MaskError::ModelMissing(checkpoint.display().to_string()));
        }
        if meta.len() < MIN_BYTES || meta.len() > MAX_BYTES {
            return Err(MaskError::ModelCorrupt(format!("{} bytes is not the size of a SAM 2.1 checkpoint", meta.len())));
        }
        let want = expected(&cfg);
        match catch_unwind(AssertUnwindSafe(|| -> Result<Self, MaskError> {
            let pth = candle_core::pickle::PthTensors::new(checkpoint, Some("model")).map_err(|e| corrupt(format!("not a SAM 2 checkpoint ({e})")))?;
            let mut tensors = HashMap::with_capacity(want.len());
            let mut total = 0usize;
            for (name, shape) in &want {
                // shape and type from the index first: a hostile entry is refused before it is decoded
                let info =
                    pth.tensor_infos().get(name).ok_or_else(|| corrupt(format!("{name} is missing (is this the checkpoint that goes with the config?)")))?;
                if info.layout.shape().dims() != shape.as_slice() {
                    return Err(corrupt(format!(
                        "{name} is {:?}, expected {shape:?} (is this the checkpoint that goes with the config?)",
                        info.layout.shape().dims()
                    )));
                }
                if !matches!(info.dtype, DType::F32 | DType::F16 | DType::BF16) {
                    return Err(corrupt(format!("{name} is stored as {:?}, not floating point", info.dtype)));
                }
                let t =
                    pth.get(name).map_err(corrupt)?.ok_or_else(|| corrupt(format!("{name} is missing (is this the checkpoint that goes with the config?)")))?;
                if t.dims() != shape.as_slice() {
                    return Err(corrupt(format!("{name} is {:?}, expected {shape:?} (is this the checkpoint that goes with the config?)", t.dims())));
                }
                total = total.saturating_add(t.elem_count());
                if total > MAX_VALUES {
                    return Err(corrupt("the checkpoint is far larger than SAM 2.1 Large"));
                }
                let t = t.to_dtype(DType::F32).map_err(corrupt)?;
                let finite = t.flatten_all().and_then(|f| f.to_vec1::<f32>()).map_err(corrupt)?.iter().all(|v| v.is_finite());
                if !finite {
                    return Err(corrupt(format!("{name} has non-finite values")));
                }
                tensors.insert(name.clone(), t);
            }
            Ok(Self { cfg, tensors })
        })) {
            Ok(r) => r,
            Err(p) => Err(MaskError::ModelCorrupt(filmcraft_ml::job::panic_message(p.as_ref()))),
        }
    }

    /// Built from tensors already in memory (tests): the same checks as [`Sam2Weights::load`].
    pub fn from_tensors(cfg: Sam2Config, tensors: HashMap<String, Tensor>) -> Result<Self, MaskError> {
        cfg.validate()?;
        for (name, shape) in expected(&cfg) {
            let t = tensors.get(&name).ok_or_else(|| corrupt(format!("{name} is missing")))?;
            if t.dims() != shape.as_slice() {
                return Err(corrupt(format!("{name} is {:?}, expected {shape:?}", t.dims())));
            }
        }
        Ok(Self { cfg, tensors })
    }

    #[cfg(test)]
    pub(crate) fn from_tensors_unchecked(cfg: Sam2Config, tensors: HashMap<String, Tensor>) -> Self {
        Self { cfg, tensors }
    }

    pub fn get(&self, name: &str) -> Result<&Tensor, MaskError> {
        self.tensors.get(name).ok_or_else(|| MaskError::Failed(format!("weight {name} is not loaded")))
    }

    /// One tensor as plain data.
    pub fn plain(&self, name: &str) -> Result<PlainTensor, MaskError> {
        let t = self.get(name)?;
        let data = t.flatten_all().and_then(|f| f.to_vec1::<f32>()).map_err(|e| MaskError::Failed(e.to_string()))?;
        Ok(PlainTensor { shape: t.dims().to_vec(), data })
    }

    /// Every tensor whose name starts with one of `prefixes`, as plain data (for the GPU).
    pub fn plain_with(&self, prefixes: &[&str]) -> Result<HashMap<String, PlainTensor>, MaskError> {
        let mut out = HashMap::new();
        for name in self.tensors.keys().filter(|n| prefixes.iter().any(|p| n.starts_with(p))) {
            out.insert(name.clone(), self.plain(name)?);
        }
        Ok(out)
    }

    /// Frees the tensors whose names start with one of `prefixes` (after a GPU upload).
    pub fn release(&mut self, prefixes: &[&str]) {
        self.tensors.retain(|n, _| !prefixes.iter().any(|p| n.starts_with(p)));
    }

    pub fn device(&self) -> Device {
        Device::Cpu
    }
}

/// Constants derived from a checkpoint that a GPU implementation uploads with the weights
/// (computed by the same code the CPU reference uses, so both agree).
#[derive(Clone, Debug, PartialEq)]
pub struct Sam2Constants {
    /// Hiera's position embedding for the 256 × 256 tokens (HWC).
    pub hiera_pos: Vec<f32>,
    /// The 64 × 64 sine position of the image embedding (4096 × 256).
    pub vision_pos: Vec<f32>,
    /// The memory attention's RoPE tables (4096 × 128 each).
    pub rope_cos: Vec<f32>,
    pub rope_sin: Vec<f32>,
}

impl Sam2Weights {
    pub fn constants(&self) -> Result<Sam2Constants, MaskError> {
        let cfg = &self.cfg;
        let bkg = self.plain("image_encoder.trunk.pos_embed")?;
        let win = self.plain("image_encoder.trunk.pos_embed_window")?;
        let hiera_pos = super::hiera_position(&bkg.data, cfg.pos_bkg.0, cfg.pos_bkg.1, &win.data, cfg.window_spec[0], cfg.embed_dim);
        let vision_pos = super::sine_position(super::EMBED_SIDE, super::EMBED_SIDE, D_MODEL);
        let (rope_cos, rope_sin) = super::rope_tables(super::EMBED_SIDE, D_MODEL, cfg.rope_theta);
        Ok(Sam2Constants { hiera_pos, vision_pos, rope_cos, rope_sin })
    }
}

/// The tensors the heavy parts use (what a GPU implementation uploads; [`Sam2Weights::release`]
/// can free them afterwards): image encoder, memory attention, memory encoder, `conv_s0/s1`.
pub const HEAVY_PREFIXES: [&str; 5] = ["image_encoder.", "memory_attention.", "memory_encoder.", "sam_mask_decoder.conv_s0.", "sam_mask_decoder.conv_s1."];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sam2::Sam2Size;

    #[test]
    fn missing_or_hostile_checkpoints_say_why() {
        let cfg = Sam2Config::builtin(Sam2Size::BasePlus);
        let dir = std::env::temp_dir().join(format!("fc-sam2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(Sam2Weights::load(&dir.join("nope.pt"), cfg.clone()), Err(MaskError::ModelMissing(_))));
        assert!(matches!(Sam2Weights::load(&dir, cfg.clone()), Err(MaskError::ModelMissing(_))));
        let small = dir.join("small.pt");
        std::fs::write(&small, b"PK nothing").unwrap();
        assert!(matches!(Sam2Weights::load(&small, cfg.clone()), Err(MaskError::ModelCorrupt(_))));
        let junk = dir.join("junk.pt");
        std::fs::write(&junk, (0..17u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect::<Vec<_>>()).unwrap();
        let e = Sam2Weights::load(&junk, cfg.clone()).unwrap_err();
        assert!(matches!(e, MaskError::ModelCorrupt(_)), "{e:?}");
        let _ = std::fs::remove_dir_all(&dir);
        // in-memory: a missing and a misshapen tensor
        assert!(Sam2Weights::from_tensors(cfg.clone(), HashMap::new()).is_err());
        let mut all: HashMap<String, Tensor> =
            expected(&cfg).into_iter().map(|(n, s)| (n, Tensor::zeros(s.as_slice(), DType::F32, &Device::Cpu).unwrap())).collect();
        assert!(Sam2Weights::from_tensors(cfg.clone(), all.clone()).is_ok());
        all.insert("no_obj_ptr".into(), Tensor::zeros((2, 3), DType::F32, &Device::Cpu).unwrap());
        assert!(Sam2Weights::from_tensors(cfg, all).unwrap_err().to_string().contains("no_obj_ptr"));
    }

    /// Both official checkpoints load against their configs (skips when not installed); the
    /// wrong config for a checkpoint is refused.
    #[test]
    fn both_official_checkpoints_load() {
        for size in [Sam2Size::BasePlus, Sam2Size::Large] {
            let Some((pt, yaml)) = crate::sam2::tests_support::files(size) else { continue };
            let cfg = Sam2Config::from_yaml(&std::fs::read_to_string(&yaml).unwrap()).unwrap();
            assert_eq!(cfg, Sam2Config::builtin(size));
            let w = Sam2Weights::load(&pt, cfg).unwrap();
            assert_eq!(w.get("image_encoder.trunk.patch_embed.proj.weight").unwrap().dims()[0], Sam2Config::builtin(size).embed_dim);
            let other = if size == Sam2Size::Large { Sam2Size::BasePlus } else { Sam2Size::Large };
            let e = Sam2Weights::load(&pt, Sam2Config::builtin(other)).unwrap_err();
            assert!(e.to_string().contains("expected"), "{e}");
        }
    }
}
