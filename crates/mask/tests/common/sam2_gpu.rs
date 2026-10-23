//! `filmcraft_gpu::sam2::Sam2Gpu` as a `filmcraft_mask::sam2::Sam2Compute` (tests and examples;
//! the engine has its own copy of this adapter, as the mask crate can't depend on the GPU
//! crate's layer).

#![allow(dead_code)]

use std::collections::HashMap;

use filmcraft_gpu::sam2::{GpuBlock, Sam2Gpu, Sam2GpuSpec, Weight};
use filmcraft_mask::MaskError;
use filmcraft_mask::sam2::weights::{HEAVY_PREFIXES, Sam2Weights};
use filmcraft_mask::sam2::{Sam2Compute, Sam2Features};

pub struct GpuCompute(pub Sam2Gpu);

fn failed(e: impl std::fmt::Display) -> MaskError {
    MaskError::Failed(e.to_string())
}

/// The GPU spec for a checkpoint.
pub fn spec(w: &Sam2Weights) -> Result<Sam2GpuSpec, MaskError> {
    let cfg = &w.cfg;
    let c = w.constants()?;
    let e = cfg.embed_dim;
    Ok(Sam2GpuSpec {
        embed_dim: e,
        blocks: cfg.blocks().iter().map(|b| GpuBlock { dim: b.dim, dim_out: b.dim_out, heads: b.heads, window: b.window, pool: b.pool, res: b.res }).collect(),
        stage_ends: cfg.stage_ends(),
        stage_dims: [e, 2 * e, 4 * e, 8 * e],
        mem_layers: cfg.mem_layers,
        mem_ffn: cfg.mem_ffn,
        fuser_layers: cfg.fuser_layers,
        hiera_pos: c.hiera_pos,
        vision_pos: c.vision_pos,
        rope_cos: c.rope_cos,
        rope_sin: c.rope_sin,
    })
}

pub fn open(w: &Sam2Weights) -> Result<GpuCompute, MaskError> {
    let spec = spec(w)?;
    let plain: HashMap<String, Weight> = w.plain_with(&HEAVY_PREFIXES)?.into_iter().map(|(k, t)| (k, Weight { shape: t.shape, data: t.data })).collect();
    Sam2Gpu::open(&spec, &plain).map(GpuCompute).map_err(failed)
}

impl Sam2Compute for GpuCompute {
    fn name(&self) -> String {
        self.0.name().to_string()
    }

    fn image(&self, input: &[f32]) -> Result<Sam2Features, MaskError> {
        let (s0, s1, feat) = self.0.image(input).map_err(failed)?;
        Ok(Sam2Features { s0, s1, feat })
    }

    fn memory_attention(&self, feat: &[f32], memory: &[f32], memory_pos: &[f32], ptr_tokens: usize) -> Result<Vec<f32>, MaskError> {
        self.0.memory_attention(feat, memory, memory_pos, ptr_tokens).map_err(failed)
    }

    fn memory_encoder(&self, feat: &[f32], mask: &[f32]) -> Result<Vec<f32>, MaskError> {
        self.0.memory_encoder(feat, mask).map_err(failed)
    }
}
