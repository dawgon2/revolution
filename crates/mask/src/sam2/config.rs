//! SAM 2.1's architecture numbers, read from the official model configs
//! (`sam2.1_hiera_l.yaml`, `sam2.1_hiera_b+.yaml`). One implementation runs both: the configs
//! differ only in the Hiera trunk (Large: 144 wide, 2-6-36-4 blocks; Base+: 112 wide, the
//! Hiera defaults 2-3-16-3) and the neck's input widths.
//!
//! The parser reads the small YAML subset those files use (nested `key: value` maps, `[a, b]`
//! lists, comments); anything else is ignored. Every number is bounded before use, so a hostile
//! config is refused with [`MaskError::ModelCorrupt`] instead of allocating or looping forever.

use std::collections::HashMap;

use crate::MaskError;

/// Better (Hiera Large) or Faster (Hiera Base+), as Resolve Studio names its Magic Mask modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sam2Size {
    /// Hiera Large: `sam2.1_hiera_large.pt` + `sam2.1_hiera_l.yaml`.
    Large,
    /// Hiera Base+: `sam2.1_hiera_base_plus.pt` + `sam2.1_hiera_b+.yaml`.
    BasePlus,
}

impl Sam2Size {
    /// The checkpoint and config file names (in `models/sam2/`).
    pub fn files(self) -> (&'static str, &'static str) {
        match self {
            Sam2Size::Large => ("sam2.1_hiera_large.pt", "sam2.1_hiera_l.yaml"),
            Sam2Size::BasePlus => ("sam2.1_hiera_base_plus.pt", "sam2.1_hiera_b+.yaml"),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Sam2Size::Large => "Better (SAM 2.1 Hiera Large)",
            Sam2Size::BasePlus => "Faster (SAM 2.1 Hiera Base+)",
        }
    }
}

/// The numbers the port needs. Fields the port can't vary (the 256-wide prompt / mask decoder,
/// the 1024 input, the 64-wide memory) are checked to be what it implements.
#[derive(Clone, Debug, PartialEq)]
pub struct Sam2Config {
    pub embed_dim: usize,
    pub num_heads: usize,
    /// Blocks per stage (4 stages).
    pub stages: [usize; 4],
    /// Blocks with global (not windowed) attention.
    pub global_att_blocks: Vec<usize>,
    /// The background position embedding's grid (h, w), bicubically resized to the 256 × 256 tokens.
    pub pos_bkg: (usize, usize),
    /// Window size per stage.
    pub window_spec: [usize; 4],
    /// Stages after which the query is pooled 2 × 2 (Hiera's `q_pool`, 3).
    pub q_pool: usize,
    /// The neck's input widths, deepest stage first (Hiera's `channel_list`).
    pub backbone_channels: [usize; 4],
    /// Memory attention layers.
    pub mem_layers: usize,
    pub mem_ffn: usize,
    pub rope_theta: f32,
    /// Memory encoder fuser blocks.
    pub fuser_layers: usize,
    /// Frames in the mask memory (the prompted frame + 6 recent ones).
    pub num_maskmem: usize,
    pub sigmoid_scale: f32,
    pub sigmoid_bias: f32,
}

/// Width of the prompt encoder / mask decoder / memory attention (SAM 2: 256).
pub const D_MODEL: usize = 256;
/// Width of a memory token (the memory encoder's output).
pub const MEM_DIM: usize = 64;
/// The encoder's input side.
pub const IMAGE_SIZE: usize = 1024;
/// Side of the image embedding (1024 / 16).
pub const EMBED_SIDE: usize = 64;
/// Most object pointers the memory attention sees (SAM 2: 16).
pub const MAX_OBJ_PTRS: usize = 16;

impl Sam2Config {
    /// Hiera's defaults (Base+), as the official `Hiera.__init__` sets them.
    fn hiera_defaults() -> Self {
        Sam2Config {
            embed_dim: 96,
            num_heads: 1,
            stages: [2, 3, 16, 3],
            global_att_blocks: vec![12, 16, 20],
            pos_bkg: (14, 14),
            window_spec: [8, 4, 14, 7],
            q_pool: 3,
            backbone_channels: [768, 384, 192, 96],
            mem_layers: 4,
            mem_ffn: 2048,
            rope_theta: 10_000.0,
            fuser_layers: 2,
            num_maskmem: 7,
            sigmoid_scale: 1.0,
            sigmoid_bias: 0.0,
        }
    }

    /// The config the official files describe, without reading them (tests, fallbacks).
    pub fn builtin(size: Sam2Size) -> Self {
        let mut c = Self::hiera_defaults();
        c.sigmoid_scale = 20.0;
        c.sigmoid_bias = -10.0;
        match size {
            Sam2Size::Large => {
                c.embed_dim = 144;
                c.num_heads = 2;
                c.stages = [2, 6, 36, 4];
                c.global_att_blocks = vec![23, 33, 43];
                c.pos_bkg = (7, 7);
                c.window_spec = [8, 4, 16, 8];
                c.backbone_channels = [1152, 576, 288, 144];
            }
            Sam2Size::BasePlus => {
                c.embed_dim = 112;
                c.num_heads = 2;
                c.backbone_channels = [896, 448, 224, 112];
            }
        }
        c
    }

    /// Parse an official SAM 2.1 config. Missing trunk fields take Hiera's defaults (as Hydra
    /// does); impossible numbers are [`MaskError::ModelCorrupt`].
    pub fn from_yaml(text: &str) -> Result<Self, MaskError> {
        if text.len() > 1 << 20 {
            return Err(bad("the model config is over 1 MB"));
        }
        let map = parse(text);
        let get = |k: &str| map.get(k).map(String::as_str);
        let mut c = Self::hiera_defaults();
        let trunk = "model.image_encoder.trunk.";
        if get(&format!("{trunk}_target_")).is_some_and(|t| !t.ends_with("hieradet.Hiera")) {
            return Err(bad("the image encoder is not Hiera"));
        }
        let num = |k: &str, d: usize| -> Result<usize, MaskError> {
            match get(k) {
                Some(v) => v.trim().parse::<usize>().map_err(|_| bad(&format!("{k} = {v:?} is not a whole number"))),
                None => Ok(d),
            }
        };
        let float = |k: &str, d: f32| -> Result<f32, MaskError> {
            match get(k) {
                Some(v) => v.trim().parse::<f32>().ok().filter(|f| f.is_finite()).ok_or_else(|| bad(&format!("{k} = {v:?} is not a number"))),
                None => Ok(d),
            }
        };
        let list = |k: &str| -> Result<Option<Vec<usize>>, MaskError> {
            match get(k) {
                Some(v) => parse_list(v).map(Some).ok_or_else(|| bad(&format!("{k} = {v:?} is not a list of whole numbers"))),
                None => Ok(None),
            }
        };
        let four = |k: &str, d: [usize; 4]| -> Result<[usize; 4], MaskError> {
            match list(k)? {
                Some(v) => <[usize; 4]>::try_from(v.as_slice()).map_err(|_| bad(&format!("{k} needs 4 entries"))),
                None => Ok(d),
            }
        };
        c.embed_dim = num(&format!("{trunk}embed_dim"), c.embed_dim)?;
        c.num_heads = num(&format!("{trunk}num_heads"), c.num_heads)?;
        c.stages = four(&format!("{trunk}stages"), c.stages)?;
        if let Some(g) = list(&format!("{trunk}global_att_blocks"))? {
            c.global_att_blocks = g;
        }
        if let Some(p) = list(&format!("{trunk}window_pos_embed_bkg_spatial_size"))? {
            match p.as_slice() {
                [h, w] => c.pos_bkg = (*h, *w),
                _ => return Err(bad("window_pos_embed_bkg_spatial_size needs 2 entries")),
            }
        }
        c.window_spec = four(&format!("{trunk}window_spec"), c.window_spec)?;
        c.q_pool = num(&format!("{trunk}q_pool"), c.q_pool)?;
        c.backbone_channels = four("model.image_encoder.neck.backbone_channel_list", c.backbone_channels)?;
        c.mem_layers = num("model.memory_attention.num_layers", c.mem_layers)?;
        c.mem_ffn = num("model.memory_attention.layer.dim_feedforward", c.mem_ffn)?;
        c.rope_theta = float("model.memory_attention.layer.self_attention.rope_theta", c.rope_theta)?;
        c.fuser_layers = num("model.memory_encoder.fuser.num_layers", c.fuser_layers)?;
        c.num_maskmem = num("model.num_maskmem", c.num_maskmem)?;
        c.sigmoid_scale = float("model.sigmoid_scale_for_mem_enc", c.sigmoid_scale)?;
        c.sigmoid_bias = float("model.sigmoid_bias_for_mem_enc", c.sigmoid_bias)?;
        // The fixed parts of the port.
        let fixed = [
            ("model.image_encoder.neck.d_model", D_MODEL),
            ("model.memory_attention.d_model", D_MODEL),
            ("model.memory_encoder.out_dim", MEM_DIM),
            ("model.image_size", IMAGE_SIZE),
            ("model.memory_encoder.fuser.layer.kernel_size", 7),
            ("model.memory_attention.layer.self_attention.num_heads", 1),
            ("model.memory_attention.layer.cross_attention.num_heads", 1),
            ("model.memory_attention.layer.cross_attention.kv_in_dim", MEM_DIM),
        ];
        for (k, want) in fixed {
            let v = num(k, want)?;
            if v != want {
                return Err(bad(&format!("{k} is {v}; this version runs {want}")));
            }
        }
        c.validate()?;
        Ok(c)
    }

    /// Bounds every number (so a hostile config can't make the port allocate or loop wildly).
    pub fn validate(&self) -> Result<(), MaskError> {
        let ok = |cond: bool, what: &str| if cond { Ok(()) } else { Err(bad(what)) };
        ok((8..=2048).contains(&self.embed_dim), "trunk width out of range")?;
        ok((1..=64).contains(&self.num_heads) && self.embed_dim.is_multiple_of(self.num_heads), "trunk heads don't divide its width")?;
        ok(self.stages.iter().all(|s| (1..=64).contains(s)), "a trunk stage has no or too many blocks")?;
        ok(self.window_spec.iter().all(|w| (1..=64).contains(w)), "a window size is out of range")?;
        ok(self.q_pool <= 3, "q_pool is over 3")?;
        ok(self.global_att_blocks.len() <= 64 && self.global_att_blocks.iter().all(|b| *b < self.depth()), "a global attention block is past the last block")?;
        ok((1..=64).contains(&self.pos_bkg.0) && (1..=64).contains(&self.pos_bkg.1), "position embedding grid out of range")?;
        // widths double per stage; heads double too and must keep dividing
        for (s, c) in self.backbone_channels.iter().rev().enumerate() {
            let want = self.embed_dim << s;
            ok(*c == want, &format!("neck input {s} is {c} wide; the trunk gives {want}"))?;
            ok(want.is_multiple_of(self.num_heads << s), "trunk heads don't divide a stage width")?;
            ok((want / (self.num_heads << s)).is_multiple_of(4), "attention head width is not a multiple of 4")?;
        }
        // stage 0 runs at 256 × 256, each pooled stage halves; windows must fit
        ok(self.window_spec.iter().zip([256usize, 128, 64, 32]).all(|(w, r)| *w <= r), "a window is larger than its stage")?;
        ok((1..=16).contains(&self.mem_layers), "memory attention layers out of range")?;
        ok((1..=16384).contains(&self.mem_ffn), "memory attention feed-forward width out of range")?;
        ok(self.rope_theta.is_finite() && self.rope_theta > 1.0, "rope theta out of range")?;
        ok((1..=8).contains(&self.fuser_layers), "memory fuser layers out of range")?;
        // the GPU memory attention is sized for 7 memory frames + 16 prompted ones
        ok((2..=7).contains(&self.num_maskmem), "memory frames out of range (this version runs up to 7)")?;
        ok(
            self.sigmoid_scale.is_finite() && self.sigmoid_bias.is_finite() && self.sigmoid_scale.abs() <= 1000.0 && self.sigmoid_bias.abs() <= 1000.0,
            "memory mask scale out of range",
        )?;
        Ok(())
    }

    /// Blocks in the trunk.
    pub fn depth(&self) -> usize {
        self.stages.iter().sum()
    }

    /// Index of each stage's last block.
    pub fn stage_ends(&self) -> [usize; 4] {
        let mut e = [0; 4];
        let mut acc = 0;
        for (i, s) in self.stages.iter().enumerate() {
            acc += s;
            if let Some(slot) = e.get_mut(i) {
                *slot = acc.saturating_sub(1);
            }
        }
        e
    }

    /// Every trunk block's shape, as `Hiera.__init__` lays them out.
    pub fn blocks(&self) -> Vec<BlockSpec> {
        let ends = self.stage_ends();
        let pool_blocks: Vec<usize> = ends.iter().take(3).map(|e| e + 1).take(self.q_pool).collect();
        let mut out = Vec::with_capacity(self.depth());
        let (mut dim, mut heads, mut stage, mut res) = (self.embed_dim, self.num_heads, 0usize, 256usize);
        for i in 0..self.depth() {
            let mut window = self.window_spec.get(stage).copied().unwrap_or(0);
            if self.global_att_blocks.contains(&i) {
                window = 0;
            }
            let mut dim_out = dim;
            if i > 0 && ends.contains(&(i - 1)) {
                dim_out = dim * 2;
                heads *= 2;
                stage += 1;
            }
            let pool = pool_blocks.contains(&i);
            out.push(BlockSpec { dim, dim_out, heads, window, pool, res, stage });
            if pool {
                res /= 2;
            }
            dim = dim_out;
        }
        out
    }
}

/// One Hiera block: `dim` in, `dim_out` out (wider at a stage's first block, which also pools
/// the query 2 × 2 when `pool`), windowed attention (`window` > 0, at the input resolution
/// `res`) or global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSpec {
    pub dim: usize,
    pub dim_out: usize,
    pub heads: usize,
    pub window: usize,
    pub pool: bool,
    /// Token grid side at the block's input.
    pub res: usize,
    pub stage: usize,
}

fn bad(what: &str) -> MaskError {
    MaskError::ModelCorrupt(format!("the SAM 2 config: {what}"))
}

/// Flattens nested `key: value` lines into dotted paths (`model.image_encoder.trunk.embed_dim`).
fn parse(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    for raw in text.lines().take(10_000) {
        let line = match raw.find('#') {
            Some(i) => raw.get(..i).unwrap_or(""),
            None => raw,
        };
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let Some((key, value)) = line.trim().split_once(':') else { continue };
        let key = key.trim().to_string();
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let path: Vec<&str> = stack.iter().map(|(_, k)| k.as_str()).chain(std::iter::once(key.as_str())).collect();
        let path = path.join(".");
        let value = value.trim();
        if value.is_empty() {
            if stack.len() < 32 {
                stack.push((indent, key));
            }
        } else {
            map.insert(path, value.trim_matches(|c| c == '"' || c == '\'').to_string());
        }
    }
    map
}

fn parse_list(v: &str) -> Option<Vec<usize>> {
    let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    inner.split(',').take(256).map(|s| s.trim().parse::<usize>().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LARGE: &str = "model:\n  _target_: sam2.modeling.sam2_base.SAM2Base\n  image_encoder:\n    scalp: 1\n    trunk:\n      _target_: sam2.modeling.backbones.hieradet.Hiera\n      embed_dim: 144\n      num_heads: 2\n      stages: [2, 6, 36, 4]\n      global_att_blocks: [23, 33, 43]\n      window_pos_embed_bkg_spatial_size: [7, 7]\n      window_spec: [8, 4, 16, 8]\n    neck:\n      d_model: 256\n      backbone_channel_list: [1152, 576, 288, 144]  # comment\n  memory_attention:\n    d_model: 256\n    layer:\n      dim_feedforward: 2048\n      self_attention:\n        rope_theta: 10000.0\n        num_heads: 1\n    num_layers: 4\n  memory_encoder:\n      out_dim: 64\n      fuser:\n        layer:\n          kernel_size: 7\n        num_layers: 2\n  num_maskmem: 7\n  sigmoid_scale_for_mem_enc: 20.0\n  sigmoid_bias_for_mem_enc: -10.0\n";

    #[test]
    fn the_official_numbers_parse_and_match_the_builtin_ones() {
        let c = Sam2Config::from_yaml(LARGE).unwrap();
        assert_eq!(c, Sam2Config::builtin(Sam2Size::Large));
        let b = Sam2Config::builtin(Sam2Size::BasePlus);
        b.validate().unwrap();
        // Large: q-pool blocks 2, 8, 44; global 23, 33, 43; first block of stage 3 has window 4
        // (stage 2's 8 from the previous stage, pooled), as Hiera does
        let blocks = c.blocks();
        assert_eq!(blocks.len(), 48);
        let pools: Vec<usize> = blocks.iter().enumerate().filter(|(_, b)| b.pool).map(|(i, _)| i).collect();
        assert_eq!(pools, vec![2, 8, 44]);
        assert_eq!(blocks[2], BlockSpec { dim: 144, dim_out: 288, heads: 4, window: 8, pool: true, res: 256, stage: 1 });
        assert_eq!(blocks[23].window, 0);
        assert_eq!(blocks[44], BlockSpec { dim: 576, dim_out: 1152, heads: 16, window: 16, pool: true, res: 64, stage: 3 });
        assert_eq!(blocks[47].res, 32);
        let bp = b.blocks();
        assert_eq!(bp.len(), 24);
        assert_eq!(bp[21], BlockSpec { dim: 448, dim_out: 896, heads: 16, window: 14, pool: true, res: 64, stage: 3 });
    }

    #[test]
    fn hostile_configs_are_refused() {
        for (from, to) in [
            ("embed_dim: 144", "embed_dim: 99999999999"),
            ("embed_dim: 144", "embed_dim: -3"),
            ("stages: [2, 6, 36, 4]", "stages: [2, 6]"),
            ("stages: [2, 6, 36, 4]", "stages: [2, 6, 3600000, 4]"),
            ("global_att_blocks: [23, 33, 43]", "global_att_blocks: [230]"),
            ("window_spec: [8, 4, 16, 8]", "window_spec: [8, 4, 0, 8]"),
            ("backbone_channel_list: [1152, 576, 288, 144]", "backbone_channel_list: [1, 2, 3, 4]"),
            ("num_maskmem: 7", "num_maskmem: 70000"),
            ("d_model: 256", "d_model: 512"),
            ("rope_theta: 10000.0", "rope_theta: NaN"),
            ("Hiera\n", "ViT\n"),
        ] {
            assert!(LARGE.contains(from), "{from}");
            let e = Sam2Config::from_yaml(&LARGE.replacen(from, to, 1));
            assert!(matches!(e, Err(MaskError::ModelCorrupt(_))), "{to}: {e:?}");
        }
        assert!(Sam2Config::from_yaml(&"x".repeat(2 << 20)).is_err());
        // garbage parses to the defaults, which are valid Hiera B numbers but not a neck match
        assert!(Sam2Config::from_yaml("{{{{ : : [[[").is_ok());
    }
}
