//! The transformer forward pass on the GPU (AI3.3b): the same model and contract as
//! [`crate::forward::Forward`], with every weight uploaded once and the activations, KV cache and
//! attention scores kept on the GPU. Only the last token's logits are read back.
//!
//! Two paths share the weights and the KV cache:
//! - **One token** (generation): one submission of every layer's kernels, built once at load as
//!   [`Dispatch`]es over fixed buffers; the position comes from a step uniform.
//! - **A batch** (prompt prefill): up to [`Self::batch`] tokens per chunk through batched
//!   RMSNorm, the tiled K-quant matmul (`gpu_matmul.rs`; a matvec per token for Q8_0 / F32
//!   weights), batched rope and KV store, a causal prefill attention kernel (each token attends to
//!   the cache up to its own position, online softmax) and batched element kernels. One
//!   submission per layer, at most two in flight, each wait bounded: a single multi-second
//!   submission loses the device on Intel's driver.
//!
//! The embedding row is looked up on the CPU from the CPU model's weights (shared, not copied)
//! and uploaded. The rotary embedding uses the CPU's cos / sin table, so positions match exactly.
//!
//! Qwen3 (AI4.2): heads wider than `embd / n_head` (`Config::q_dim`) and the per-head q / k
//! RMSNorm before the rotary embedding (`head_norm` kernel). Qwen3-VL's multimodal prompt
//! ([`Self::forward_multimodal`], the CPU's contract): picture tokens take the encoder's rows as
//! input, every token's M-RoPE angles are computed on the CPU (`crate::mrope::angles`, as the CPU
//! path) and uploaded as per-token cos / sin rows the rope kernel reads instead of the position
//! table, and a DeepStack tap's rows are added to the picture tokens' residuals after its layer.
//!
//! Numerics: the GPU multiplies f32 activations by the weights directly, where the CPU quantizes
//! activations to 8 bits (Q8_K / Q8_0) first; logits therefore differ by roughly the CPU's own
//! quantization error (see the parity tests), not by a GPU error.
//!
//! Every failure (no adapter, too little memory, a validation error, a lost device) is an
//! [`LlmError`]; the caller ([`crate::Model`]) falls back to the CPU.

use std::sync::Arc;

use crate::LlmError;
use crate::forward::{Config, Forward, MmPicture, MmToken, Rope};
use crate::gpu::{Dispatch, GpuContext, GpuMatrix, Op};
use crate::quant::err;
use crate::weights::Weight;

/// Head dimensions the attention kernels hold in workgroup memory.
const MAX_HEAD_DIM: usize = 512;
/// Most prompt tokens per batched chunk (the batch buffers hold this many; SmolLM3-3B: ~70 MB).
const PREFILL_BATCH: usize = 512;
/// Prompt tokens per [`GpuForward::forward`] call the caller should use between deadline checks
/// on an integrated GPU (a discrete one takes [`GpuForward::batch`]).
const INTEGRATED_STEP: usize = 64;

pub struct GpuForward {
    ctx: GpuContext,
    cfg: Config,
    /// The CPU model's embedding (row lookups).
    embd: Arc<Weight>,
    /// `[pos, n_ctx]` of the (first) token being run.
    step: wgpu::Buffer,
    /// The residual stream; the new token's embedding is written here.
    x: wgpu::Buffer,
    logits: wgpu::Buffer,
    /// One token through every layer: `[layer][kernel]`, the rotary embedding from the position
    /// table at the step's position.
    token_ops: Vec<Vec<Dispatch>>,
    /// The same with the rotary embedding from row 0 of the per-token angles (M-RoPE).
    token_ops_mm: Vec<Vec<Dispatch>>,
    /// The final norm and the output head (reads `x`).
    head: Vec<Dispatch>,
    /// What the batched path binds per layer.
    layers: Vec<LayerGpu>,
    /// The batched path's activations.
    batch: Option<Batch>,
    /// Tokens per batched chunk (≤ the batch buffers' size); 0 runs prompts token by token.
    batch_len: usize,
    /// Uniforms the batched path shares across layers.
    shared: Shared,
    /// Matrices and buffers the dispatches bind (kept for their lifetime and the byte count).
    matrices: Vec<GpuMatrix>,
    _buffers: Vec<wgpu::Buffer>,
    bytes: u64,
    len: usize,
    /// Test hook: dropping sleeps this long (as dropping a hung device's queue blocks).
    #[cfg(test)]
    pub(crate) slow_drop: Option<SlowDrop>,
}

/// A layer's buffers and matrices (indices into [`GpuForward::matrices`]).
struct LayerGpu {
    attn_norm: wgpu::Buffer,
    ffn_norm: wgpu::Buffer,
    kc: wgpu::Buffer,
    vc: wgpu::Buffer,
    bias: Option<[wgpu::Buffer; 3]>,
    /// Qwen3's per-head q / k norm weights (`head_dim` each).
    qk_norm: Option<[wgpu::Buffer; 2]>,
    rope: bool,
    ffn: usize,
    wq: usize,
    wk: usize,
    wv: usize,
    wo: usize,
    gate: usize,
    up: usize,
    down: usize,
}

/// Activations of a batch of `cap` tokens, token-major.
struct Batch {
    cap: usize,
    x: wgpu::Buffer,
    h: wgpu::Buffer,
    q: wgpu::Buffer,
    k: wgpu::Buffer,
    v: wgpu::Buffer,
    att: wgpu::Buffer,
    proj: wgpu::Buffer,
    g: wgpu::Buffer,
    u: wgpu::Buffer,
}

/// Parameter uniforms that do not depend on the batch length.
struct Shared {
    rms: wgpu::Buffer,
    /// Rope params reading the position table (`table`).
    rope_q: wgpu::Buffer,
    rope_k: wgpu::Buffer,
    /// Rope params reading the per-token angles (`angles`: `angles_cap × half` cos values, then
    /// as many sin values).
    rope_q_mm: wgpu::Buffer,
    rope_k_mm: wgpu::Buffer,
    /// A step uniform fixed at position 0, so the rope kernel's row is the token's index in the
    /// batch (the angles buffer's row).
    zero_step: wgpu::Buffer,
    angles: wgpu::Buffer,
    angles_cap: usize,
    /// `[head_dim, eps, n_head]` / `[head_dim, eps, n_kv_head]` for the q / k norm.
    norm_q: wgpu::Buffer,
    norm_k: wgpu::Buffer,
    /// `[embd, 0]`: an add over one token's residual.
    n_embd: wgpu::Buffer,
    kv: wgpu::Buffer,
    att: wgpu::Buffer,
    /// The rope cos / sin table.
    table: wgpu::Buffer,
    kv_dim: usize,
    half: usize,
}

/// Where a batch's rotary angles come from.
enum Angles<'a> {
    /// The position table at the step's positions (text-only prompts).
    Table,
    /// Per-token cos / sin rows (`n × half` each): M-RoPE.
    Rows { cos: &'a [f32], sin: &'a [f32] },
}

/// Sleeps on drop (test hook for [`GpuForward`]).
#[cfg(test)]
pub(crate) struct SlowDrop(pub(crate) std::time::Duration);

/// Slow drops still sleeping (a test waits for them before it ends: a process exiting while a
/// thread is still tearing a GPU device down crashed in NVIDIA's driver on the XPS 16).
#[cfg(test)]
pub(crate) static SLOW_DROPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
impl SlowDrop {
    pub(crate) fn new(d: std::time::Duration) -> SlowDrop {
        SLOW_DROPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SlowDrop(d)
    }

    /// Wait (at most `limit`) until every slow drop has finished.
    pub(crate) fn wait_all(limit: std::time::Duration) -> bool {
        let t = std::time::Instant::now();
        while SLOW_DROPS.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            if t.elapsed() > limit {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        true
    }
}

#[cfg(test)]
impl Drop for SlowDrop {
    fn drop(&mut self) {
        std::thread::sleep(self.0);
        SLOW_DROPS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for GpuForward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuForward").field("adapter", &self.ctx.name()).field("bytes", &self.bytes).field("len", &self.len).finish_non_exhaustive()
    }
}

fn u32_of(v: usize, what: &str) -> Result<u32, LlmError> {
    u32::try_from(v).map_err(|_| err(format!("{what} {v} is too large for the GPU")))
}

/// Builds the buffers and dispatches; keeps what they bind.
struct Builder<'a> {
    ctx: &'a GpuContext,
    matrices: Vec<GpuMatrix>,
    buffers: Vec<wgpu::Buffer>,
    bytes: u64,
}

impl Builder<'_> {
    fn storage(&mut self, n: usize, label: &str) -> Result<wgpu::Buffer, LlmError> {
        let b = self.ctx.storage(n, label)?;
        self.bytes = self.bytes.saturating_add(b.size());
        Ok(b)
    }
    fn init(&mut self, data: &[f32], label: &str) -> Result<wgpu::Buffer, LlmError> {
        let b = self.ctx.storage_init(data, label)?;
        self.bytes = self.bytes.saturating_add(b.size());
        Ok(b)
    }
    fn uniform(&mut self, words: &[u32]) -> wgpu::Buffer {
        let b = self.ctx.uniform(words);
        self.buffers.push(b.clone());
        b
    }
    fn matrix(&mut self, w: &Weight) -> Result<usize, LlmError> {
        let m = self.ctx.upload_weight(w)?;
        self.bytes = self.bytes.saturating_add(m.bytes());
        self.matrices.push(m);
        Ok(self.matrices.len() - 1)
    }
    fn matvec(&self, m: usize, x: &wgpu::Buffer, y: &wgpu::Buffer) -> Result<Dispatch, LlmError> {
        let m = self.matrices.get(m).ok_or_else(|| err("matrix index"))?;
        self.ctx.matvec_dispatch(m, x, y)
    }
    fn op(&self, op: Op, bufs: &[&wgpu::Buffer], groups: [u32; 2]) -> Result<Dispatch, LlmError> {
        self.ctx.op_dispatch(op, bufs, groups)
    }
    fn keep(&mut self, bufs: &[&wgpu::Buffer]) {
        self.buffers.extend(bufs.iter().map(|b| (*b).clone()));
    }
}

/// The one-token activations every layer's kernels bind.
struct TokenBufs {
    x: wgpu::Buffer,
    h: wgpu::Buffer,
    q: wgpu::Buffer,
    k: wgpu::Buffer,
    v: wgpu::Buffer,
    att: wgpu::Buffer,
    proj: wgpu::Buffer,
    g: wgpu::Buffer,
    u: wgpu::Buffer,
    scores: wgpu::Buffer,
    step: wgpu::Buffer,
    n_q: wgpu::Buffer,
    n_kv: wgpu::Buffer,
    att_p: wgpu::Buffer,
}

/// Tokens per batched chunk the adapter's binding limit allows (`widest` f32 per token in the
/// largest batch buffer), at most `max_pos` and [`PREFILL_BATCH`].
fn batch_cap(max_binding: u64, widest: usize, max_pos: usize) -> usize {
    let per_token = (widest.max(1) as u64).saturating_mul(4);
    let fit = usize::try_from(max_binding / per_token).unwrap_or(usize::MAX);
    fit.min(PREFILL_BATCH).min(max_pos)
}

impl GpuForward {
    /// Uploads `f`'s weights to the default high-performance adapter. An error (no GPU, a matrix
    /// over the adapter's binding limit, out of GPU memory, …) means: use `f` on the CPU.
    pub fn load(f: &Forward) -> Result<GpuForward, LlmError> {
        Self::with_context(GpuContext::new()?, f)
    }

    /// [`Self::load`] on an existing context.
    pub fn with_context(ctx: GpuContext, f: &Forward) -> Result<GpuForward, LlmError> {
        let c = f.cfg.clone();
        let kv_dim = c.n_kv_head.checked_mul(c.head_dim).ok_or_else(|| err("KV width"))?;
        if c.n_head == 0
            || c.n_kv_head == 0
            || !c.n_head.is_multiple_of(c.n_kv_head)
            || c.head_dim == 0
            || c.head_dim > MAX_HEAD_DIM
            || !c.head_dim.is_multiple_of(2)
            || c.n_head.checked_mul(c.head_dim) != Some(c.q_dim)
            || c.embd == 0
            || c.max_pos == 0
            || c.n_layer == 0
            || f.layers.len() != c.n_layer
            || f.layers.iter().any(|l| l.qk_norm.is_some() != c.qk_norm || l.bias.is_some() != c.qkv_bias)
            || f.layers.iter().flat_map(|l| l.qk_norm.iter().flatten()).any(|w| w.len() != c.head_dim)
        {
            return Err(err(format!("GPU forward: unsupported shape {c:?}")));
        }
        let half = c.head_dim / 2;
        let table_len = c.max_pos.checked_mul(half).ok_or_else(|| err("rope table"))?;
        if f.cos.len() != table_len || f.sin.len() != table_len {
            return Err(err("rope table size"));
        }
        let ffn = f.layers.iter().map(|l| l.gate.rows).max().unwrap_or(0);
        let mut b = Builder { ctx: &ctx, matrices: Vec::new(), buffers: Vec::new(), bytes: 0 };
        let x = b.storage(c.embd, "llm-x")?;
        let logits = b.storage(c.vocab, "llm-logits")?;
        let step = b.uniform(&[0, 1]);
        let (hd, n_head_u, n_kv_u) = (u32_of(c.head_dim, "head dim")?, u32_of(c.n_head, "heads")?, u32_of(c.n_kv_head, "KV heads")?);
        let max_pos = u32_of(c.max_pos, "context")?;
        let sqrt_dim = (c.head_dim as f32).sqrt();
        let att_words = [n_head_u, n_kv_u, hd, max_pos, u32_of(kv_dim, "KV width")?, sqrt_dim.to_bits()];
        let tb = TokenBufs {
            x: x.clone(),
            h: b.storage(c.embd, "llm-h")?,
            q: b.storage(c.q_dim, "llm-q")?,
            k: b.storage(kv_dim, "llm-k")?,
            v: b.storage(kv_dim, "llm-v")?,
            att: b.storage(c.q_dim, "llm-att")?,
            proj: b.storage(c.embd, "llm-proj")?,
            g: b.storage(ffn, "llm-gate")?,
            u: b.storage(ffn, "llm-up")?,
            scores: b.storage(c.n_head.checked_mul(c.max_pos).ok_or_else(|| err("scores"))?, "llm-scores")?,
            step: step.clone(),
            n_q: b.uniform(&[u32_of(c.q_dim, "query width")?]),
            n_kv: b.uniform(&[u32_of(kv_dim, "KV width")?]),
            att_p: b.uniform(&att_words),
        };
        let mut table = Vec::new();
        table.try_reserve_exact(table_len.saturating_mul(2)).map_err(|_| err("out of memory for the rope table"))?;
        table.extend_from_slice(&f.cos);
        table.extend_from_slice(&f.sin);
        let table = b.init(&table, "llm-rope")?;
        b.keep(&[&tb.x, &tb.h, &tb.q, &tb.k, &tb.v, &tb.att, &tb.proj, &tb.g, &tb.u, &logits, &tb.scores, &table]);

        // the batched path's activations; without room for them, prompts run token by token
        let cap = batch_cap(ctx.max_binding(), ffn.max(c.embd).max(c.q_dim), c.max_pos);
        let angles_cap = cap.max(1);
        let angles = b.storage(angles_cap.checked_mul(half).and_then(|n| n.checked_mul(2)).ok_or_else(|| err("angles"))?, "llm-angles")?;
        b.keep(&[&angles]);

        let embd_u = u32_of(c.embd, "embedding")?;
        let rms = b.uniform(&[embd_u, c.eps.to_bits()]);
        let n_embd = b.uniform(&[embd_u]);
        let interleaved = u32::from(c.rope == Rope::Interleaved);
        let sin_at = u32_of(table_len, "rope table")?;
        let rope_q = b.uniform(&[n_head_u, hd, interleaved, sin_at]);
        let rope_k = b.uniform(&[n_kv_u, hd, interleaved, sin_at]);
        let sin_at_mm = u32_of(angles_cap.saturating_mul(half), "angles")?;
        let rope_q_mm = b.uniform(&[n_head_u, hd, interleaved, sin_at_mm]);
        let rope_k_mm = b.uniform(&[n_kv_u, hd, interleaved, sin_at_mm]);
        let zero_step = b.uniform(&[0, 1]);
        let norm_q = b.uniform(&[hd, c.eps.to_bits(), n_head_u]);
        let norm_k = b.uniform(&[hd, c.eps.to_bits(), n_kv_u]);
        let kv_p = b.uniform(&[u32_of(kv_dim, "KV width")?, max_pos]);
        let att_batch = b.uniform(&[att_words.as_slice(), &[u32_of(c.q_dim, "query width")?]].concat());
        let shared = Shared {
            rms,
            rope_q,
            rope_k,
            rope_q_mm,
            rope_k_mm,
            zero_step,
            angles,
            angles_cap,
            norm_q,
            norm_k,
            n_embd,
            kv: kv_p,
            att: att_batch,
            table,
            kv_dim,
            half,
        };
        let kv_cache_len = c.max_pos.checked_mul(kv_dim).ok_or_else(|| err("KV cache size"))?;

        let mut layers = Vec::with_capacity(f.layers.len());
        let (mut token_ops, mut token_ops_mm) = (Vec::with_capacity(f.layers.len()), Vec::with_capacity(f.layers.len()));
        for (i, l) in f.layers.iter().enumerate() {
            if l.up.rows != l.gate.rows || l.down.cols != l.gate.rows {
                return Err(err(format!("layer {i}: MLP shapes differ")));
            }
            let attn_norm = b.init(&l.attn_norm, "llm-attn-norm")?;
            let ffn_norm = b.init(&l.ffn_norm, "llm-ffn-norm")?;
            let kc = b.storage(kv_cache_len, "llm-k-cache")?;
            let vc = b.storage(kv_cache_len, "llm-v-cache")?;
            b.keep(&[&attn_norm, &ffn_norm, &kc, &vc]);
            let (wq, wk, wv, wo) = (b.matrix(&l.wq)?, b.matrix(&l.wk)?, b.matrix(&l.wv)?, b.matrix(&l.wo)?);
            let (gate, up, down) = (b.matrix(&l.gate)?, b.matrix(&l.up)?, b.matrix(&l.down)?);
            let mut bias = None;
            if let Some([bq, bk, bv]) = &l.bias {
                let (bq, bk, bv) = (b.init(bq, "llm-bq")?, b.init(bk, "llm-bk")?, b.init(bv, "llm-bv")?);
                b.keep(&[&bq, &bk, &bv]);
                bias = Some([bq, bk, bv]);
            }
            let mut qk_norm = None;
            if let Some([qn, kn]) = &l.qk_norm {
                let (qn, kn) = (b.init(qn, "llm-q-norm")?, b.init(kn, "llm-k-norm")?);
                b.keep(&[&qn, &kn]);
                qk_norm = Some([qn, kn]);
            }
            let layer = LayerGpu { attn_norm, ffn_norm, kc, vc, bias, qk_norm, rope: l.rope, ffn: l.gate.rows, wq, wk, wv, wo, gate, up, down };
            token_ops.push(Self::token_layer(&b, &c, &shared, &tb, &layer, false)?);
            token_ops_mm.push(Self::token_layer(&b, &c, &shared, &tb, &layer, true)?);
            layers.push(layer);
            if i % 4 == 3 {
                // surface a failure early instead of uploading the rest of the model
                ctx.check()?;
            }
        }
        let norm = b.init(&f.norm, "llm-norm")?;
        b.keep(&[&norm]);
        let out = b.matrix(f.output.as_ref().unwrap_or(&*f.embd))?;
        let head = vec![b.op(Op::RmsNorm, &[&shared.rms, &x, &norm, &tb.h], [1, 1])?, b.matvec(out, &tb.h, &logits)?];

        let batch = if cap >= 2 {
            let n = |w: usize| cap.saturating_mul(w);
            Some(Batch {
                cap,
                x: b.storage(n(c.embd), "llm-bx")?,
                h: b.storage(n(c.embd), "llm-bh")?,
                q: b.storage(n(c.q_dim), "llm-bq")?,
                k: b.storage(n(kv_dim), "llm-bk")?,
                v: b.storage(n(kv_dim), "llm-bv")?,
                att: b.storage(n(c.q_dim), "llm-batt")?,
                proj: b.storage(n(c.embd), "llm-bproj")?,
                g: b.storage(n(ffn), "llm-bg")?,
                u: b.storage(n(ffn), "llm-bu")?,
            })
        } else {
            None
        };
        ctx.check()?;
        let Builder { matrices, buffers, bytes, .. } = b;
        log::info!(
            "LLM on the GPU ({}): {} MB uploaded, {} kernels per token, prompt batches of {cap}",
            ctx.name(),
            bytes / 1_000_000,
            token_ops.iter().map(Vec::len).sum::<usize>().saturating_add(head.len())
        );
        Ok(GpuForward {
            ctx,
            cfg: c,
            embd: f.embd.clone(),
            step,
            x,
            logits,
            token_ops,
            token_ops_mm,
            head,
            layers,
            batch_len: batch.as_ref().map_or(0, |b| b.cap),
            batch,
            shared,
            matrices,
            _buffers: buffers,
            bytes,
            len: 0,
            #[cfg(test)]
            slow_drop: None,
        })
    }

    /// One token through layer `l`, in order; `mm`: the rotary embedding from row 0 of the
    /// per-token angles instead of the position table.
    fn token_layer(b: &Builder<'_>, c: &Config, s: &Shared, t: &TokenBufs, l: &LayerGpu, mm: bool) -> Result<Vec<Dispatch>, LlmError> {
        let ctx = b.ctx;
        let g_embd = ctx.groups(c.embd, 64)?;
        let g_q = ctx.groups(c.q_dim, 64)?;
        let g_kv = ctx.groups(s.kv_dim, 64)?;
        let g_rope_q = ctx.groups(c.n_head.saturating_mul(s.half), 64)?;
        let g_rope_k = ctx.groups(c.n_kv_head.saturating_mul(s.half), 64)?;
        let g_att = [u32_of(c.n_head, "heads")?, 1];
        let mut ops = vec![b.op(Op::RmsNorm, &[&s.rms, &t.x, &l.attn_norm, &t.h], [1, 1])?];
        ops.push(b.matvec(l.wq, &t.h, &t.q)?);
        ops.push(b.matvec(l.wk, &t.h, &t.k)?);
        ops.push(b.matvec(l.wv, &t.h, &t.v)?);
        if let Some([bq, bk, bv]) = &l.bias {
            ops.push(b.op(Op::Add, &[&t.n_q, &t.q, bq], g_q)?);
            ops.push(b.op(Op::Add, &[&t.n_kv, &t.k, bk], g_kv)?);
            ops.push(b.op(Op::Add, &[&t.n_kv, &t.v, bv], g_kv)?);
        }
        if let Some([qn, kn]) = &l.qk_norm {
            ops.push(b.op(Op::HeadNorm, &[&s.norm_q, qn, &t.q], [u32_of(c.n_head, "heads")?, 1])?);
            ops.push(b.op(Op::HeadNorm, &[&s.norm_k, kn, &t.k], [u32_of(c.n_kv_head, "KV heads")?, 1])?);
        }
        if l.rope {
            if mm {
                ops.push(b.op(Op::Rope, &[&s.rope_q_mm, &s.zero_step, &s.angles, &t.q], g_rope_q)?);
                ops.push(b.op(Op::Rope, &[&s.rope_k_mm, &s.zero_step, &s.angles, &t.k], g_rope_k)?);
            } else {
                ops.push(b.op(Op::Rope, &[&s.rope_q, &t.step, &s.table, &t.q], g_rope_q)?);
                ops.push(b.op(Op::Rope, &[&s.rope_k, &t.step, &s.table, &t.k], g_rope_k)?);
            }
        }
        ops.push(b.op(Op::KvStore, &[&s.kv, &t.step, &t.k, &t.v, &l.kc, &l.vc], g_kv)?);
        ops.push(b.op(Op::Attention, &[&t.att_p, &t.step, &t.q, &l.kc, &l.vc, &t.scores, &t.att], g_att)?);
        ops.push(b.matvec(l.wo, &t.att, &t.proj)?);
        ops.push(b.op(Op::Add, &[&s.n_embd, &t.x, &t.proj], g_embd)?);
        ops.push(b.op(Op::RmsNorm, &[&s.rms, &t.x, &l.ffn_norm, &t.h], [1, 1])?);
        ops.push(b.matvec(l.gate, &t.h, &t.g)?);
        ops.push(b.matvec(l.up, &t.h, &t.u)?);
        let n_ffn = ctx.uniform(&[u32_of(l.ffn, "MLP width")?]);
        ops.push(b.op(Op::SiluMul, &[&n_ffn, &t.g, &t.u], ctx.groups(l.ffn, 64)?)?);
        ops.push(b.matvec(l.down, &t.g, &t.proj)?);
        ops.push(b.op(Op::Add, &[&s.n_embd, &t.x, &t.proj], g_embd)?);
        Ok(ops)
    }

    /// The adapter's name.
    pub fn adapter(&self) -> &str {
        self.ctx.name()
    }

    /// Bytes of weights, caches and activations on the GPU.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Most prompt tokens one batched chunk runs (0: prompts run token by token).
    pub fn batch(&self) -> usize {
        self.batch_len
    }

    /// Prompt tokens per [`Self::forward`] call between the caller's deadline checks: a batch on
    /// a discrete GPU (~0.3 s for SmolLM3-3B), fewer on an integrated one (≈ 1 s each).
    pub fn prefill_step(&self) -> usize {
        match self.batch_len {
            0 => 16,
            b if self.ctx.is_discrete() => b,
            b => b.min(INTEGRATED_STEP),
        }
    }

    /// Whether the device failed (an error it reported, a lost device, a timed-out wait), as
    /// opposed to an input or shape this forward pass refused.
    pub fn device_failed(&self) -> bool {
        self.ctx.check().is_err()
    }

    /// Forget the KV cache (a new conversation).
    pub fn reset(&mut self) {
        self.len = 0;
    }

    /// Keep only the first `len` cached tokens (no-op when fewer are cached).
    pub fn truncate(&mut self, len: usize) {
        self.len = self.len.min(len);
    }

    /// Tokens currently in the KV cache.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Runs `tokens` at positions `pos..` and returns the logits after the last one. `pos` must
    /// equal [`Self::len`] (continue) or be 0 (start over). On an error the cache is forgotten.
    pub fn forward(&mut self, tokens: &[u32], pos: usize) -> Result<Vec<f32>, LlmError> {
        self.check_input(tokens, pos)?;
        if pos == 0 {
            self.reset();
        }
        let r = self.run(tokens, pos);
        self.len = if r.is_ok() { pos.saturating_add(tokens.len()) } else { 0 };
        r
    }

    /// Whether [`Self::forward`] accepts `tokens` at `pos` (any error it returns past this check
    /// is the GPU's): a position that continues the cache or restarts at 0, ids inside the
    /// vocabulary, the context not exceeded.
    pub fn check_input(&self, tokens: &[u32], pos: usize) -> Result<(), LlmError> {
        if pos != 0 && pos != self.len {
            return Err(err(format!("position {pos}, but {} tokens are cached", self.len)));
        }
        let end = pos.checked_add(tokens.len()).filter(|e| *e <= self.cfg.max_pos);
        if tokens.is_empty() || end.is_none() {
            return Err(err(format!("{} tokens at {pos} exceed the context of {}", tokens.len(), self.cfg.max_pos)));
        }
        if let Some(t) = tokens.iter().find(|t| usize::try_from(**t).map_or(true, |t| t >= self.cfg.vocab)) {
            return Err(err(format!("token id {t} is outside the vocabulary of {}", self.cfg.vocab)));
        }
        Ok(())
    }

    /// Qwen3-VL: the GPU's [`Forward::forward_multimodal`], with the same arguments and
    /// contract. Appends `toks` (text tokens and a picture's merged tokens) after the cached
    /// ones, each at its own (t, h, w) position from [`crate::mrope::positions`] with the
    /// config's M-RoPE `sections`, and returns the logits after the last. A picture token's input
    /// is its row of `picture.tokens`; after decoder layer l < `picture.deepstack.len()` its
    /// hidden state gets that tap's row added (DeepStack). Text-only calls (positions from the
    /// previous call's `next`) continue a conversation: one token runs on the one-token path. An
    /// input this pass refuses ([`Self::check_multimodal`]) keeps the cache; a GPU error forgets
    /// it. Mix with [`Self::forward`] only through [`Self::reset`].
    pub fn forward_multimodal(
        &mut self,
        toks: &[MmToken],
        positions: &[[u32; 3]],
        sections: [usize; 3],
        picture: Option<MmPicture<'_>>,
    ) -> Result<Vec<f32>, LlmError> {
        self.check_multimodal(toks, positions, sections, picture.as_ref())?;
        let pos = self.len;
        let r = self.run_multimodal(toks, positions, sections, picture, pos);
        self.len = if r.is_ok() { pos.saturating_add(toks.len()) } else { 0 };
        r
    }

    /// Whether [`Self::forward_multimodal`] accepts these inputs (the CPU's checks): as many
    /// positions as tokens, the context not exceeded, a halves-rotary model whose `sections` fill
    /// half a head, text ids inside the vocabulary, every picture token's row present in the
    /// encoder output and in every DeepStack tap.
    pub fn check_multimodal(&self, toks: &[MmToken], positions: &[[u32; 3]], sections: [usize; 3], picture: Option<&MmPicture<'_>>) -> Result<(), LlmError> {
        let c = &self.cfg;
        let end = self.len.checked_add(toks.len()).filter(|e| *e <= c.max_pos);
        if toks.is_empty() || end.is_none() || positions.len() != toks.len() {
            return Err(err(format!("{} tokens / {} positions at {} (context {})", toks.len(), positions.len(), self.len, c.max_pos)));
        }
        if c.rope != Rope::Halves || sections.iter().try_fold(0usize, |a, s| a.checked_add(*s)) != Some(c.head_dim / 2) {
            return Err(err("multimodal positions need a halves-rotary model whose sections fill half a head"));
        }
        let mut need = None;
        for t in toks {
            match *t {
                MmToken::Text(id) => {
                    if usize::try_from(id).map_or(true, |t| t >= c.vocab) {
                        return Err(err(format!("token id {id} is outside the vocabulary of {}", c.vocab)));
                    }
                }
                MmToken::Picture(k) => need = Some(need.map_or(k, |n: usize| n.max(k))),
            }
        }
        if let Some(need) = need {
            let rows = need.checked_add(1).and_then(|n| n.checked_mul(c.embd)).ok_or_else(|| err("picture token number"))?;
            let p = picture.ok_or_else(|| err("a picture token without a picture"))?;
            if p.tokens.len() < rows || p.deepstack.iter().any(|t| t.len() < rows) {
                return Err(err(format!("picture token {need} past the encoder's output")));
            }
        }
        Ok(())
    }

    fn run_multimodal(
        &mut self,
        toks: &[MmToken],
        positions: &[[u32; 3]],
        sections: [usize; 3],
        picture: Option<MmPicture<'_>>,
        pos: usize,
    ) -> Result<Vec<f32>, LlmError> {
        let c = self.cfg.clone();
        let (embd, half) = (c.embd, c.head_dim / 2);
        let pic_row = |k: usize, data: &[f32]| -> Result<Vec<f32>, LlmError> {
            let at = k.checked_mul(embd).ok_or_else(|| err("picture token number"))?;
            data.get(at..at.saturating_add(embd)).map(<[f32]>::to_vec).ok_or_else(|| err(format!("picture token {k} past the encoder's output")))
        };
        let input = |t: MmToken, embd_w: &Weight| -> Result<Vec<f32>, LlmError> {
            match t {
                MmToken::Text(id) => embd_w.row(usize::try_from(id).map_err(|_| err("token id"))?),
                MmToken::Picture(k) => pic_row(k, picture.as_ref().ok_or_else(|| err("a picture token without a picture"))?.tokens),
            }
        };
        let taps: &[Vec<f32>] = picture.as_ref().map_or(&[], |p| p.deepstack);
        let n_taps = taps.len().min(c.n_layer);
        let angles_of = |p: [u32; 3]| -> (Vec<f32>, Vec<f32>) {
            let a = crate::mrope::angles(p, c.head_dim, sections, c.rope_base);
            (a.iter().map(|x| x.cos()).collect(), a.iter().map(|x| x.sin()).collect())
        };
        let embd_w = self.embd.clone();
        let logits = if toks.len() >= 2 && self.batch_len >= 2 {
            let mut at = pos;
            for (chunk, pchunk) in toks.chunks(self.batch_len).zip(positions.chunks(self.batch_len)) {
                let n = chunk.len();
                let mut rows = Vec::new();
                rows.try_reserve_exact(n.saturating_mul(embd)).map_err(|_| err("out of memory for the prompt embeddings"))?;
                let (mut cos, mut sin) = (Vec::with_capacity(n.saturating_mul(half)), Vec::with_capacity(n.saturating_mul(half)));
                for (t, p) in chunk.iter().zip(pchunk) {
                    rows.extend(input(*t, &embd_w)?);
                    let (c, s) = angles_of(*p);
                    cos.extend(c);
                    sin.extend(s);
                }
                // DeepStack: a tap's rows at the picture tokens, zeros at the text tokens
                let mut deep = Vec::with_capacity(n_taps);
                if chunk.iter().any(|t| matches!(t, MmToken::Picture(_))) {
                    for tap in taps.iter().take(n_taps) {
                        let mut d = Vec::new();
                        d.try_reserve_exact(n.saturating_mul(embd)).map_err(|_| err("out of memory for the DeepStack rows"))?;
                        for t in chunk {
                            match *t {
                                MmToken::Picture(k) => d.extend(pic_row(k, tap)?),
                                MmToken::Text(_) => d.resize(d.len().saturating_add(embd), 0.0),
                            }
                        }
                        deep.push(Some(d));
                    }
                }
                self.run_batch(&rows, at, Angles::Rows { cos: &cos, sin: &sin }, &deep)?;
                at = at.saturating_add(n);
            }
            let mut enc = self.ctx.encoder();
            self.ctx.record(&mut enc, &self.head);
            self.ctx.submit_read(enc, &self.logits, c.vocab)?
        } else {
            let mut logits = Vec::new();
            for (i, (t, p)) in toks.iter().zip(positions).enumerate() {
                let row = input(*t, &embd_w)?;
                let (cos, sin) = angles_of(*p);
                let deep: Vec<Option<Vec<f32>>> = match *t {
                    MmToken::Picture(k) => taps.iter().take(n_taps).map(|tap| pic_row(k, tap).map(Some)).collect::<Result<_, _>>()?,
                    MmToken::Text(_) => Vec::new(),
                };
                let p = u32_of(pos.saturating_add(i), "position")?;
                if let Some(l) = self.run_one(&row, p, Some((&cos, &sin)), &deep, i + 1 == toks.len())? {
                    logits = l;
                }
            }
            logits
        };
        if logits.len() != c.vocab || logits.iter().any(|v| !v.is_finite()) {
            return Err(err("the GPU returned missing or non-finite logits"));
        }
        Ok(logits)
    }

    fn run(&mut self, tokens: &[u32], pos: usize) -> Result<Vec<f32>, LlmError> {
        let logits = if tokens.len() >= 2 && self.batch_len >= 2 {
            let mut at = pos;
            for chunk in tokens.chunks(self.batch_len) {
                let mut rows = Vec::new();
                rows.try_reserve_exact(chunk.len().saturating_mul(self.cfg.embd)).map_err(|_| err("out of memory for the prompt embeddings"))?;
                for &t in chunk {
                    let t = usize::try_from(t).map_err(|_| err("token id"))?;
                    rows.extend(self.embd.row(t)?);
                }
                self.run_batch(&rows, at, Angles::Table, &[])?;
                at = at.saturating_add(chunk.len());
            }
            let mut enc = self.ctx.encoder();
            self.ctx.record(&mut enc, &self.head);
            self.ctx.submit_read(enc, &self.logits, self.cfg.vocab)?
        } else {
            self.run_one_by_one(tokens, pos)?
        };
        if logits.len() != self.cfg.vocab || logits.iter().any(|v| !v.is_finite()) {
            return Err(err("the GPU returned missing or non-finite logits"));
        }
        Ok(logits)
    }

    /// Every token as its own submission (generation, and prompts without batch buffers).
    fn run_one_by_one(&mut self, tokens: &[u32], pos: usize) -> Result<Vec<f32>, LlmError> {
        let mut logits = Vec::new();
        for (i, &t) in tokens.iter().enumerate() {
            let t = usize::try_from(t).map_err(|_| err("token id"))?;
            let row = self.embd.row(t)?;
            let p = u32_of(pos.saturating_add(i), "position")?;
            if let Some(l) = self.run_one(&row, p, None, &[], i + 1 == tokens.len())? {
                logits = l;
            }
        }
        Ok(logits)
    }

    /// One token (input `row`) at position `p` through every layer as one submission; `angles`:
    /// its rotary cos / sin (`half` each; None: the position table); `deep[l]`: a row added to
    /// the residual after layer `l` (DeepStack). With `read`, also the head: the logits.
    fn run_one(&mut self, row: &[f32], p: u32, angles: Option<(&[f32], &[f32])>, deep: &[Option<Vec<f32>>], read: bool) -> Result<Option<Vec<f32>>, LlmError> {
        let s = &self.shared;
        if row.len() != self.cfg.embd {
            return Err(err("input row width"));
        }
        self.ctx.write(&self.x, 0, row)?;
        self.ctx.write_u32(&self.step, &[p, p.saturating_add(1)])?;
        let ops = if let Some((cos, sin)) = angles {
            if cos.len() != s.half || sin.len() != s.half {
                return Err(err("rotary angles don't match the head"));
            }
            self.ctx.write(&s.angles, 0, cos)?;
            self.ctx.write(&s.angles, s.angles_cap.saturating_mul(s.half), sin)?;
            &self.token_ops_mm
        } else {
            &self.token_ops
        };
        let g_embd = self.ctx.groups(self.cfg.embd, 64)?;
        let mut enc = self.ctx.encoder();
        for (li, layer) in ops.iter().enumerate() {
            self.ctx.record(&mut enc, layer);
            if let Some(Some(d)) = deep.get(li) {
                if d.len() != self.cfg.embd {
                    return Err(err("DeepStack row width"));
                }
                let buf = self.ctx.storage_init(d, "llm-deepstack")?;
                let add = self.ctx.op_dispatch(Op::Add, &[&s.n_embd, &self.x, &buf], g_embd)?;
                self.ctx.record(&mut enc, std::slice::from_ref(&add));
            }
        }
        if read {
            self.ctx.record(&mut enc, &self.head);
            return self.ctx.submit_read(enc, &self.logits, self.cfg.vocab).map(Some);
        }
        // one token in flight at a time: a long queue of slow submissions can make a driver
        // expire its fences and drop work without reporting an error (seen on i915 with a UHD
        // 750: ~1.5 s per token, a 74-token prompt queued)
        let index = self.ctx.submit(enc)?;
        self.ctx.wait(Some(index))?;
        Ok(None)
    }

    /// `rows` (`n × embd` input embeddings, n ≤ the batch capacity) at positions `pos..` through
    /// every layer, batched, with their rotary `angles`; `deep[l]` (`n × embd`) is added to the
    /// residuals after layer `l`. Their keys and values land in the caches and the last token's
    /// residual in `x` (for the head).
    fn run_batch(&mut self, rows: &[f32], pos: usize, angles: Angles<'_>, deep: &[Option<Vec<f32>>]) -> Result<(), LlmError> {
        let Some(bt) = &self.batch else { return Err(err("no GPU batch buffers")) };
        let c = &self.cfg;
        let n = rows.len() / c.embd.max(1);
        if n == 0 || n > bt.cap || !rows.len().is_multiple_of(c.embd) {
            return Err(err(format!("a batch of {n} tokens (capacity {})", bt.cap)));
        }
        let n32 = u32_of(n, "batch")?;
        self.ctx.write(&bt.x, 0, rows)?;
        let p = u32_of(pos, "position")?;
        self.ctx.write_u32(&self.step, &[p, p.saturating_add(n32)])?;
        let ctx = &self.ctx;
        let s = &self.shared;
        let n_embd = n.checked_mul(c.embd).ok_or_else(|| err("batch size"))?;
        let n_q = n.checked_mul(c.q_dim).ok_or_else(|| err("batch size"))?;
        let add_x = ctx.uniform(&[u32_of(n_embd, "batch")?, 0]);
        let g_x = ctx.groups_2d(n_embd, 64)?;
        let rope_q = [ctx.groups(c.n_head.saturating_mul(s.half), 64)?[0], n32];
        let rope_k = [ctx.groups(c.n_kv_head.saturating_mul(s.half), 64)?[0], n32];
        let g_kv = [ctx.groups(s.kv_dim, 64)?[0], n32];
        let g_norm_q = [u32_of(c.n_head, "heads")?, n32];
        let g_norm_k = [u32_of(c.n_kv_head, "KV heads")?, n32];
        // the rotary embedding's source: the position table at the step, or the uploaded rows
        let (rope_q_p, rope_k_p, rope_step, rope_table) = match angles {
            Angles::Table => (&s.rope_q, &s.rope_k, &self.step, &s.table),
            Angles::Rows { cos, sin } => {
                let want = n.saturating_mul(s.half);
                if cos.len() != want || sin.len() != want || n > s.angles_cap {
                    return Err(err("rotary angles don't match the batch"));
                }
                ctx.write(&s.angles, 0, cos)?;
                ctx.write(&s.angles, s.angles_cap.saturating_mul(s.half), sin)?;
                (&s.rope_q_mm, &s.rope_k_mm, &s.zero_step, &s.angles)
            }
        };
        let mut deep_bufs = Vec::with_capacity(deep.len());
        for d in deep {
            deep_bufs.push(match d {
                Some(d) if d.len() == n_embd => Some(ctx.storage_init(d, "llm-deepstack")?),
                Some(_) => return Err(err("DeepStack rows don't match the batch")),
                None => None,
            });
        }
        // eight tokens per workgroup where the head dimension allows it
        let (att_op, g_att) = if c.head_dim <= 256 && c.head_dim.is_multiple_of(4) {
            (Op::AttentionPrefill8, [u32_of(c.n_head, "heads")?, n32.div_ceil(8)])
        } else {
            (Op::AttentionPrefill, [u32_of(c.n_head, "heads")?, n32])
        };
        let bias_uniforms = if self.layers.iter().any(|l| l.bias.is_some()) {
            let n_kv = n.checked_mul(s.kv_dim).ok_or_else(|| err("batch size"))?;
            Some((
                ctx.uniform(&[u32_of(n_q, "batch")?, u32_of(c.q_dim, "query width")?]),
                ctx.groups_2d(n_q, 64)?,
                ctx.uniform(&[u32_of(n_kv, "batch")?, u32_of(s.kv_dim, "KV width")?]),
                ctx.groups_2d(n_kv, 64)?,
            ))
        } else {
            None
        };
        // the SiLU gate's length and groups per MLP width (one width in every supported model)
        let mut silu_by_width: Vec<(usize, wgpu::Buffer, [u32; 2])> = Vec::new();
        for l in &self.layers {
            if !silu_by_width.iter().any(|(w, ..)| *w == l.ffn) {
                let n_ffn = n.checked_mul(l.ffn).ok_or_else(|| err("batch size"))?;
                silu_by_width.push((l.ffn, ctx.uniform(&[u32_of(n_ffn, "batch")?]), ctx.groups_2d(n_ffn, 64)?));
            }
        }
        let mat = |i: usize| self.matrices.get(i).ok_or_else(|| err("matrix index"));
        let last = self.layers.len().saturating_sub(1);
        let mut in_flight: Option<wgpu::SubmissionIndex> = None;
        for (li, l) in self.layers.iter().enumerate() {
            let (_, silu, g_silu) = silu_by_width.iter().find(|(w, ..)| *w == l.ffn).ok_or_else(|| err("MLP width"))?;
            let mut ops = vec![ctx.op_dispatch(Op::RmsNorm, &[&s.rms, &bt.x, &l.attn_norm, &bt.h], [n32, 1])?];
            let mut enc = ctx.encoder();
            ctx.record(&mut enc, &ops);
            ctx.record_batch(&mut enc, mat(l.wq)?, &bt.h, &bt.q, n32)?;
            ctx.record_batch(&mut enc, mat(l.wk)?, &bt.h, &bt.k, n32)?;
            ctx.record_batch(&mut enc, mat(l.wv)?, &bt.h, &bt.v, n32)?;
            ops.clear();
            if let (Some([bq, bk, bv]), Some((pq, g_bq, pkv, g_bkv))) = (&l.bias, &bias_uniforms) {
                ops.push(ctx.op_dispatch(Op::Add, &[pq, &bt.q, bq], *g_bq)?);
                ops.push(ctx.op_dispatch(Op::Add, &[pkv, &bt.k, bk], *g_bkv)?);
                ops.push(ctx.op_dispatch(Op::Add, &[pkv, &bt.v, bv], *g_bkv)?);
            }
            if let Some([qn, kn]) = &l.qk_norm {
                ops.push(ctx.op_dispatch(Op::HeadNorm, &[&s.norm_q, qn, &bt.q], g_norm_q)?);
                ops.push(ctx.op_dispatch(Op::HeadNorm, &[&s.norm_k, kn, &bt.k], g_norm_k)?);
            }
            if l.rope {
                ops.push(ctx.op_dispatch(Op::Rope, &[rope_q_p, rope_step, rope_table, &bt.q], rope_q)?);
                ops.push(ctx.op_dispatch(Op::Rope, &[rope_k_p, rope_step, rope_table, &bt.k], rope_k)?);
            }
            ops.push(ctx.op_dispatch(Op::KvStore, &[&s.kv, &self.step, &bt.k, &bt.v, &l.kc, &l.vc], g_kv)?);
            ops.push(ctx.op_dispatch(att_op, &[&s.att, &self.step, &bt.q, &l.kc, &l.vc, &bt.att], g_att)?);
            ctx.record(&mut enc, &ops);
            ctx.record_batch(&mut enc, mat(l.wo)?, &bt.att, &bt.proj, n32)?;
            ops.clear();
            ops.push(ctx.op_dispatch(Op::Add, &[&add_x, &bt.x, &bt.proj], g_x)?);
            ops.push(ctx.op_dispatch(Op::RmsNorm, &[&s.rms, &bt.x, &l.ffn_norm, &bt.h], [n32, 1])?);
            ctx.record(&mut enc, &ops);
            ctx.record_batch(&mut enc, mat(l.gate)?, &bt.h, &bt.g, n32)?;
            ctx.record_batch(&mut enc, mat(l.up)?, &bt.h, &bt.u, n32)?;
            ops.clear();
            ops.push(ctx.op_dispatch(Op::SiluMul, &[silu, &bt.g, &bt.u], *g_silu)?);
            ctx.record(&mut enc, &ops);
            ctx.record_batch(&mut enc, mat(l.down)?, &bt.g, &bt.proj, n32)?;
            ops.clear();
            ops.push(ctx.op_dispatch(Op::Add, &[&add_x, &bt.x, &bt.proj], g_x)?);
            // DeepStack: the picture's tap for this layer joins its tokens' residuals
            if let Some(Some(d)) = deep_bufs.get(li) {
                ops.push(ctx.op_dispatch(Op::Add, &[&add_x, &bt.x, d], g_x)?);
            }
            ctx.record(&mut enc, &ops);
            if li == last {
                let row = (c.embd as u64).saturating_mul(4);
                enc.copy_buffer_to_buffer(&bt.x, row.saturating_mul(n.saturating_sub(1) as u64), &self.x, 0, row);
            }
            // one layer per submission, at most two in flight
            let index = ctx.submit(enc)?;
            if let Some(prev) = in_flight.replace(index) {
                ctx.wait(Some(prev))?;
            }
        }
        if let Some(prev) = in_flight {
            ctx.wait(Some(prev))?;
        }
        Ok(())
    }
}

#[cfg(test)]
impl GpuForward {
    /// Fails the device as a lost GPU would.
    pub(crate) fn poison(&self) {
        self.ctx.fail("poisoned by a test");
    }

    pub(crate) fn ctx_for_tests(&self) -> &GpuContext {
        &self.ctx
    }

    /// Tokens per batched chunk, at most the batch buffers' size; 0 runs prompts token by token.
    pub(crate) fn set_batch(&mut self, n: usize) {
        self.batch_len = n.min(self.batch.as_ref().map_or(0, |b| b.cap));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::forward::{MmPicture, MmToken};
    use crate::gguf::tests::Builder as Gguf;
    use crate::kquant::{Q4K_BYTES, Q6K_BYTES};
    use half::f16;
    use std::io::Cursor;

    const F32: u32 = 0;
    pub(crate) const Q8_0: u32 = 8;
    const Q4_K: u32 = 12;
    const Q6_K: u32 = 14;

    struct Rng(u64);
    impl Rng {
        fn byte(&mut self) -> u8 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 56) as u8
        }
        fn unit(&mut self) -> f32 {
            f32::from(u16::from_le_bytes([self.byte(), self.byte()])) / 65_536.0 - 0.5
        }
    }

    /// A matrix's GGUF bytes in `ty`, its values roughly uniform with standard deviation ~0.06.
    fn matrix(ty: u32, rows: usize, cols: usize, rng: &mut Rng) -> Vec<u8> {
        let n = rows * cols;
        match ty {
            Q4_K => {
                let mut v: Vec<u8> = (0..n / 256 * Q4K_BYTES).map(|_| rng.byte()).collect();
                for b in v.as_chunks_mut::<Q4K_BYTES>().0 {
                    // value = d·scale·q − dmin·min: dmin = 7.5 d centres it
                    b[..2].copy_from_slice(&f16::from_f32(2e-4).to_le_bytes());
                    b[2..4].copy_from_slice(&f16::from_f32(1.5e-3).to_le_bytes());
                }
                v
            }
            Q6_K => {
                let mut v: Vec<u8> = (0..n / 256 * Q6K_BYTES).map(|_| rng.byte()).collect();
                for b in v.as_chunks_mut::<Q6K_BYTES>().0 {
                    b[Q6K_BYTES - 2..].copy_from_slice(&f16::from_f32(5e-5).to_le_bytes());
                }
                v
            }
            Q8_0 => {
                let vals: Vec<f32> = (0..n).map(|_| rng.unit() * 0.2).collect();
                let mut q = Vec::new();
                crate::quant::quantize_q8_0(&vals, &mut q).unwrap();
                q.iter().flat_map(|b| f16::from_f32(b.d).to_le_bytes().into_iter().chain(b.qs.iter().map(|x| x.to_le_bytes()[0]))).collect()
            }
            _ => (0..n).flat_map(|_| (rng.unit() * 0.2).to_le_bytes()).collect(),
        }
    }

    /// A synthetic model: embd 256, 4 heads of 64 (2 KV heads), 4 layers, MLP 512, vocabulary
    /// 512. `types[j]` is the format of the j-th matrix kind, rotated by layer, so every layer
    /// mixes formats; the token embedding (tied output) is `types[0]`. `arch` "smollm3" makes
    /// layer 3 a NoPE layer (interleaved rope elsewhere); "qwen2" adds biases, halves rope.
    /// `bytes` of type `ty` as F32 bytes of the same (dequantized) values.
    fn dequantized(ty: u32, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        match ty {
            Q4_K => {
                let mut v = Vec::new();
                crate::kquant::parse_q4k(bytes, &mut v).unwrap();
                crate::kquant::dequantize_q4k(&v, &mut out);
            }
            Q6_K => {
                let mut v = Vec::new();
                crate::kquant::parse_q6k(bytes, &mut v).unwrap();
                crate::kquant::dequantize_q6k(&v, &mut out);
            }
            Q8_0 => {
                let mut v = Vec::new();
                crate::quant::parse_q8_0(bytes, &mut v).unwrap();
                crate::quant::dequantize_q8_0(&v, &mut out);
            }
            _ => return bytes.to_vec(),
        }
        out.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    pub(crate) fn model(arch: &str, types: [u32; 4], seed: u64) -> Forward {
        model_as(arch, types, seed, false)
    }

    /// `dequant`: the same model with every matrix stored as F32 (the exact values the quantized
    /// blocks hold), so the CPU runs it without quantizing activations. A context of 64.
    fn model_as(arch: &str, types: [u32; 4], seed: u64, dequant: bool) -> Forward {
        model_ctx(arch, types, seed, dequant, 64)
    }

    /// [`model_as`] with a context of `max_pos` tokens.
    fn model_ctx(arch: &str, types: [u32; 4], seed: u64, dequant: bool, max_pos: usize) -> Forward {
        model_shaped(arch, types, seed, dequant, max_pos, (4, 2, 64))
    }

    /// [`model_ctx`] with `heads` = (query heads, KV heads, head dimension); the embedding is
    /// heads × head dimension (K-quant and Q8_0 matrices need it a multiple of 256 / 32).
    fn model_shaped(arch: &str, types: [u32; 4], seed: u64, dequant: bool, max_pos: usize, heads: (usize, usize, usize)) -> Forward {
        model_full(arch, types, seed, dequant, max_pos, heads, None)
    }

    /// The Qwen3 shape: embd 256 but 4 heads of 128 (q 512 wide, 2 KV heads: 256), per-head q / k
    /// norms, no biases, halves rope; a context of `max_pos`.
    pub(crate) fn qwen3_model(types: [u32; 4], seed: u64, dequant: bool, max_pos: usize) -> Forward {
        model_full("qwen3", types, seed, dequant, max_pos, (4, 2, 128), Some(256))
    }

    /// [`model_shaped`] with the embedding width `embd` (default heads × head dimension); "qwen3"
    /// states its head width (`key_length`) and adds the q / k norm weights.
    fn model_full(arch: &str, types: [u32; 4], seed: u64, dequant: bool, max_pos: usize, heads: (usize, usize, usize), embd: Option<usize>) -> Forward {
        let (n_head, n_kv, hd) = heads;
        let (q_dim, kv, ffn, vocab, layers) = (n_head * hd, n_kv * hd, 512usize, 512usize, 4u32);
        let embd = embd.unwrap_or(q_dim);
        let mut rng = Rng(seed);
        let mut b = Gguf::default()
            .str("general.architecture", arch)
            .u32(&format!("{arch}.embedding_length"), embd as u32)
            .u32(&format!("{arch}.attention.head_count"), n_head as u32)
            .u32(&format!("{arch}.attention.head_count_kv"), n_kv as u32)
            .u32(&format!("{arch}.block_count"), layers)
            .u32(&format!("{arch}.context_length"), 256)
            .u32(&format!("{arch}.rope.dimension_count"), hd as u32)
            .f32(&format!("{arch}.attention.layer_norm_rms_epsilon"), 1e-6)
            .f32(&format!("{arch}.rope.freq_base"), 10_000.0);
        if arch == "qwen3" {
            b = b.u32("qwen3.attention.key_length", hd as u32).u32("qwen3.attention.value_length", hd as u32);
        }
        let mat = |b: Gguf, name: &str, ty: u32, rows: usize, cols: usize, rng: &mut Rng| {
            let m = matrix(ty, rows, cols, rng);
            if dequant {
                b.tensor_data(name, &[cols as u64, rows as u64], F32, &dequantized(ty, &m))
            } else {
                b.tensor_data(name, &[cols as u64, rows as u64], ty, &m)
            }
        };
        b = mat(b, "token_embd.weight", types[0], vocab, embd, &mut rng);
        let mats = [
            ("attn_q", q_dim, embd),
            ("attn_k", kv, embd),
            ("attn_v", kv, embd),
            ("attn_output", embd, q_dim),
            ("ffn_gate", ffn, embd),
            ("ffn_up", ffn, embd),
            ("ffn_down", embd, ffn),
        ];
        let vec = |b: Gguf, name: &str, n: usize, base: f32, rng: &mut Rng| {
            let v: Vec<u8> = (0..n).flat_map(|_| (base + rng.unit() * 0.2).to_le_bytes()).collect();
            b.tensor_data(name, &[n as u64], F32, &v)
        };
        b = vec(b, "output_norm.weight", embd, 1.0, &mut rng);
        for i in 0..layers as usize {
            for (j, (name, rows, cols)) in mats.iter().enumerate() {
                let ty = types[(i + j) % 4];
                b = mat(b, &format!("blk.{i}.{name}.weight"), ty, *rows, *cols, &mut rng);
            }
            b = vec(b, &format!("blk.{i}.attn_norm.weight"), embd, 1.0, &mut rng);
            b = vec(b, &format!("blk.{i}.ffn_norm.weight"), embd, 1.0, &mut rng);
            if arch == "qwen2" {
                b = vec(b, &format!("blk.{i}.attn_q.bias"), embd, 0.0, &mut rng);
                b = vec(b, &format!("blk.{i}.attn_k.bias"), kv, 0.0, &mut rng);
                b = vec(b, &format!("blk.{i}.attn_v.bias"), kv, 0.0, &mut rng);
            }
            if arch == "qwen3" {
                b = vec(b, &format!("blk.{i}.attn_q_norm.weight"), hd, 1.0, &mut rng);
                b = vec(b, &format!("blk.{i}.attn_k_norm.weight"), hd, 1.0, &mut rng);
            }
        }
        let bytes = b.build();
        let mut c = Cursor::new(&bytes);
        let s = crate::gguf::check(&mut c, bytes.len() as u64).unwrap();
        let cfg = Config::from_gguf(&s, arch, max_pos).unwrap();
        Forward::load(&mut c, &s, cfg).unwrap()
    }

    fn argmax(v: &[f32]) -> usize {
        v.iter().enumerate().fold((0, f32::NEG_INFINITY), |m, (i, &x)| if x > m.1 { (i, x) } else { m }).0
    }

    /// Worst |gpu − cpu| over the largest |cpu| logit.
    fn rel(gpu: &[f32], cpu: &[f32]) -> f32 {
        assert_eq!(gpu.len(), cpu.len());
        let scale = cpu.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
        gpu.iter().zip(cpu).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale
    }

    fn context() -> Option<GpuContext> {
        match GpuContext::new() {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("SKIPPED: {e}");
                None
            }
        }
    }

    /// Runs the same steps on both and returns the worst relative logit difference; with `top`,
    /// asserts the top token agrees at every step.
    fn compare(cpu: &mut Forward, gpu: &mut GpuForward, what: &str, top: bool) -> f32 {
        let toks: [u32; 10] = [5, 17, 3, 88, 42, 9, 300, 511, 0, 77];
        let mut worst = 0f32;
        // a 4-token prompt, then single tokens, then a restart with a 3-token prompt and a truncate
        let mut steps: Vec<(&[u32], usize)> = vec![(&toks[..4], 0)];
        steps.extend((4..toks.len()).map(|i| (&toks[i..i + 1], i)));
        steps.push((&toks[2..5], 0));
        steps.push((&toks[7..9], 3));
        for (i, (t, pos)) in steps.iter().enumerate() {
            let want = cpu.forward(t, *pos).unwrap();
            let got = gpu.forward(t, *pos).unwrap();
            assert_eq!(gpu.len(), cpu.len());
            let r = rel(&got, &want);
            worst = worst.max(r);
            if top {
                assert_eq!(argmax(&got), argmax(&want), "{what} step {i}: top token (diff {r})");
            }
        }
        cpu.truncate(2);
        gpu.truncate(2);
        let (want, got) = (cpu.forward(&toks[5..6], 2).unwrap(), gpu.forward(&toks[5..6], 2).unwrap());
        worst = worst.max(rel(&got, &want));
        eprintln!("{what}: worst logit difference {worst:.2e} of the largest logit");
        worst
    }

    /// All-F32 weights: the CPU does not quantize activations for F32 matrices, so both sides do
    /// the same f32 arithmetic and differ only in summation order (≈ 1e-6 relative per op).
    #[test]
    fn f32_model_matches_the_cpu_closely() {
        let Some(ctx) = context() else { return };
        for arch in ["smollm3", "qwen2"] {
            let mut cpu = model(arch, [F32; 4], 7);
            if arch == "smollm3" {
                assert_eq!(cpu.layers.iter().map(|l| l.rope).collect::<Vec<_>>(), [true, true, true, false]);
            }
            let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
            let worst = compare(&mut cpu, &mut gpu, &format!("{arch} F32 on {}", ctx.name()), true);
            assert!(worst <= 1e-4, "{arch}: {worst}");
        }
    }

    /// Mixed Q4_K / Q6_K / Q8_0 / F32 weights. Two checks:
    ///
    /// 1. Against the CPU running the same model with every matrix dequantized to F32 (the exact
    ///    values the blocks hold): the same f32 arithmetic, so ≤ 1e-4 of the largest logit
    ///    (measured ~1e-6), which proves the GPU's block decoding and data flow.
    /// 2. Against the CPU on the quantized model, the production reference. The CPU quantizes each
    ///    activation vector to 8 bits (Q8_K: one scale per 256 values; Q8_0: per 32) before every
    ///    quantized product; the GPU keeps f32. So the GPU must be no further from the quantized
    ///    CPU than the CPU's own f32 run is (its activation-quantization error, a few % of the
    ///    largest logit on this random model; × 1.5 + 1e-3 for the restart steps it does not
    ///    measure). The top token is not compared here: this random model has near-ties that the
    ///    CPU's own rounding flips (check 1 compares it against the exact values).
    #[test]
    fn quantized_model_matches_the_cpu() {
        let Some(ctx) = context() else { return };
        for (arch, types) in [("smollm3", [Q4_K, Q6_K, Q8_0, F32]), ("qwen2", [Q6_K, Q8_0, Q4_K, F32]), ("smollm3", [Q8_0, Q4_K, Q4_K, Q6_K])] {
            let what = format!("{arch} {types:?} on {}", ctx.name());
            let mut exact = model_as(arch, types, 11, true);
            let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &model(arch, types, 11)).unwrap();
            let worst = compare(&mut exact, &mut gpu, &format!("{what} vs CPU f32"), true);
            assert!(worst <= 1e-4, "{what}: {worst}");
            let (mut cpu, mut exact) = (model(arch, types, 11), model_as(arch, types, 11, true));
            let cpu_err = steps_diff(&mut cpu, &mut exact);
            let mut cpu = model(arch, types, 11);
            let worst = compare(&mut cpu, &mut gpu, &format!("{what} vs CPU quantized (CPU's own quantization error {cpu_err:.2e})"), false);
            assert!(worst <= 1.5 * cpu_err + 1e-3, "{what}: {worst} vs the CPU's own {cpu_err}");
        }
    }

    /// The worst relative logit difference between two CPU models over [`compare`]'s main steps.
    fn steps_diff(a: &mut Forward, b: &mut Forward) -> f32 {
        let toks: [u32; 10] = [5, 17, 3, 88, 42, 9, 300, 511, 0, 77];
        let mut worst = rel(&a.forward(&toks[..4], 0).unwrap(), &b.forward(&toks[..4], 0).unwrap());
        for i in 4..toks.len() {
            worst = worst.max(rel(&a.forward(&toks[i..i + 1], i).unwrap(), &b.forward(&toks[i..i + 1], i).unwrap()));
        }
        worst
    }

    /// SmolLM3-3B Q4_K_M on the CPU and the GPU: the same chat prompt, then 24 greedy steps fed
    /// with the CPU's tokens (teacher forcing, so one late disagreement cannot derail the rest);
    /// prints prompt and generation speeds. `FILMCRAFT_LLM_DIR=<dir holding smollm3-3b-q4km/>
    /// cargo test --release -p filmcraft-llm --features gpu --lib gpu_forward::tests::real_model --
    /// --ignored --nocapture` (select the adapter with MESA_VK_DEVICE_SELECT).
    #[test]
    #[ignore = "needs the real model; run by hand with --release"]
    fn real_model_matches_the_cpu() {
        use std::io::BufReader;
        let Some(dir) = std::env::var_os("FILMCRAFT_LLM_DIR") else {
            eprintln!("FILMCRAFT_LLM_DIR not set; skipped");
            return;
        };
        let m = crate::models::find("smollm3-3b-q4km").unwrap();
        let d = crate::models::model_dir(std::path::Path::new(&dir), m);
        if !d.join(m.weights).exists() {
            eprintln!("{} missing; skipped", d.join(m.weights).display());
            return;
        }
        let tok = tokenizers::Tokenizer::from_file(d.join(m.tokenizer)).unwrap();
        let text = crate::prompt::chat_prompt(
            m.family,
            "You are a friendly video-editing coach. Answer briefly.",
            "How do I make a J-cut between two clips on the timeline, and why would I use one?",
        );
        let ids = tok.encode(text.as_str(), false).unwrap().get_ids().to_vec();
        let file = std::fs::File::open(d.join(m.weights)).unwrap();
        let size = file.metadata().unwrap().len();
        let mut r = BufReader::new(file);
        let s = crate::gguf::check(&mut r, size).unwrap();
        let cfg = Config::from_gguf(&s, m.family.architecture(), crate::MAX_CONTEXT).unwrap();
        let mut cpu = Forward::load(&mut r, &s, cfg).unwrap();
        let t = std::time::Instant::now();
        let mut gpu = GpuForward::load(&cpu).unwrap();
        let name = gpu.adapter().to_string();
        eprintln!("{name}: uploaded {} MB in {:.2} s", gpu.bytes() / 1_000_000, t.elapsed().as_secs_f64());
        // warm up (pipeline caches, first submission), then time the prompt on each
        gpu.forward(&ids[..2], 0).unwrap();
        let t = std::time::Instant::now();
        let lg = gpu.forward(&ids, 0).unwrap();
        let gpu_prompt = ids.len() as f64 / t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        cpu.forward(&ids, 0).unwrap();
        let cpu_prompt = ids.len() as f64 / t.elapsed().as_secs_f64();
        // the same prompt token by token (the path before batching)
        let batch = gpu.batch();
        gpu.set_batch(0);
        let t = std::time::Instant::now();
        let lone = gpu.forward(&ids, 0).unwrap();
        let one_prompt = ids.len() as f64 / t.elapsed().as_secs_f64();
        gpu.set_batch(batch);
        eprintln!("{name}: batched vs one-token GPU prompt: worst logit difference {:.2e}", rel(&lone, &lg));
        // a long prompt (~800 tokens: an assistant prompt with retrieved guide text)
        let long: Vec<u32> = ids.iter().cycle().take(800).copied().collect();
        let t = std::time::Instant::now();
        let ll = gpu.forward(&long, 0).unwrap();
        let gpu_long = t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let cl = cpu.forward(&long, 0).unwrap();
        let cpu_long = t.elapsed().as_secs_f64();
        eprintln!(
            "{name}: 800-token prompt GPU {gpu_long:.2} s ({:.0} tok/s), CPU {cpu_long:.2} s ({:.0} tok/s); top token agrees: {}, difference {:.3}",
            800.0 / gpu_long,
            800.0 / cpu_long,
            argmax(&ll) == argmax(&cl),
            rel(&ll, &cl)
        );
        // back to the chat prompt for the generation comparison
        let mut lg = gpu.forward(&ids, 0).unwrap();
        let mut lc = cpu.forward(&ids, 0).unwrap();
        let (steps, mut agree, mut worst) = (24usize, 0usize, rel(&lg, &lc));
        let (mut gpu_time, mut cpu_time) = (0f64, 0f64);
        let mut out = Vec::new();
        for pos in (ids.len()..).take(steps) {
            let (tc, tg) = (argmax(&lc), argmax(&lg));
            agree += usize::from(tc == tg);
            out.push(tc as u32);
            let t = std::time::Instant::now();
            lg = gpu.forward(&[tc as u32], pos).unwrap();
            gpu_time += t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            lc = cpu.forward(&[tc as u32], pos).unwrap();
            cpu_time += t.elapsed().as_secs_f64();
            worst = worst.max(rel(&lg, &lc));
        }
        eprintln!("answer (CPU greedy): {:?}", tok.decode(&out, true).unwrap());
        // generation on the GPU alone (the loop above idles it during every CPU step)
        let at = ids.len() + steps;
        let t = std::time::Instant::now();
        for (i, &tk) in out.iter().enumerate() {
            gpu.forward(&[tk], at + i).unwrap();
        }
        eprintln!("{name}: generation on the GPU alone {:.1} tok/s", out.len() as f64 / t.elapsed().as_secs_f64());
        eprintln!("{name}: {} prompt tokens; top token agrees on {agree} of {steps} steps; worst logit difference {worst:.3} of the largest logit", ids.len());
        eprintln!(
            "{name}: prompt GPU {gpu_prompt:.1} tok/s batched ({one_prompt:.1} token by token), CPU {cpu_prompt:.1} tok/s; generation GPU {:.1} tok/s, CPU {:.1} tok/s",
            steps as f64 / gpu_time,
            steps as f64 / cpu_time
        );
        assert!(agree >= 20, "top token agrees on {agree} of {steps}");
        assert!(worst <= 0.25, "worst logit difference {worst}");
    }

    /// Per-matrix parity and timing of SmolLM3-3B's layer 0 and output head (one round trip each),
    /// for kernel work: `FILMCRAFT_LLM_DIR=… cargo test --release -p filmcraft-llm --features gpu
    /// --lib gpu_forward::tests::real_model_matrices -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the real model; run by hand with --release"]
    fn real_model_matrices() {
        use std::io::BufReader;
        let Some(dir) = std::env::var_os("FILMCRAFT_LLM_DIR") else {
            eprintln!("FILMCRAFT_LLM_DIR not set; skipped");
            return;
        };
        let m = crate::models::find("smollm3-3b-q4km").unwrap();
        let d = crate::models::model_dir(std::path::Path::new(&dir), m);
        let Ok(file) = std::fs::File::open(d.join(m.weights)) else {
            eprintln!("{} missing; skipped", d.join(m.weights).display());
            return;
        };
        let size = file.metadata().unwrap().len();
        let mut r = BufReader::new(file);
        let s = crate::gguf::check(&mut r, size).unwrap();
        let cfg = Config::from_gguf(&s, m.family.architecture(), crate::MAX_CONTEXT).unwrap();
        let cpu = Forward::load(&mut r, &s, cfg).unwrap();
        let Some(ctx) = context() else { return };
        eprintln!("{} max_binding {}", ctx.name(), ctx.max_binding());
        let l = &cpu.layers[0];
        let mut scratch = crate::weights::Scratch::default();
        for w in [&l.wq, &l.wk, &l.wv, &l.wo, &l.gate, &l.up, &l.down, &*cpu.embd] {
            let gm = ctx.upload_weight(w).unwrap();
            let x: Vec<f32> = (0..w.cols).map(|i| ((i * 37 % 101) as f32 - 50.0) / 25.0).collect();
            let mut want = vec![0.0; w.rows];
            w.matvec(&x, &mut want, &mut scratch).unwrap();
            let t = std::time::Instant::now();
            let got = ctx.matvec(&gm, &x).unwrap();
            let ms = t.elapsed().as_secs_f64() * 1e3;
            let kind = match w.data() {
                crate::weights::Data::F32(_) => "F32",
                crate::weights::Data::Q8_0(_) => "Q8_0",
                crate::weights::Data::Q4K(_) => "Q4K",
                crate::weights::Data::Q6K(_) => "Q6K",
            };
            eprintln!("{} {kind} {}x{}: rel {:.2e}, {ms:.2} ms", w.name, w.rows, w.cols, rel(&got, &want));
        }
    }

    /// Where a token's time goes on Qwen3-VL 4B's text model (AI4.2 matvec work): the whole
    /// one-token submission against its matvecs alone and its other kernels alone, plus the
    /// output head, each the mean of several submissions after a 64-token prompt.
    /// `FILMCRAFT_QWEN3VL_DIR=… [FILMCRAFT_QWEN3VL_GGUF=text-own-q8_0.gguf] cargo test --release
    /// -p filmcraft-llm --features gpu --lib gpu_forward::tests::token_profile -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the real model; run by hand with --release"]
    fn token_profile() {
        use std::io::BufReader;
        let Some(dir) = std::env::var_os("FILMCRAFT_QWEN3VL_DIR") else {
            eprintln!("FILMCRAFT_QWEN3VL_DIR not set; skipped");
            return;
        };
        let name = std::env::var("FILMCRAFT_QWEN3VL_GGUF").unwrap_or_else(|_| "text-own-q4k.gguf".into());
        let Ok(file) = std::fs::File::open(std::path::Path::new(&dir).join(&name)) else {
            eprintln!("{name} missing; skipped");
            return;
        };
        let size = file.metadata().unwrap().len();
        let mut r = BufReader::new(file);
        let s = crate::gguf::check(&mut r, size).unwrap();
        let cfg = Config::from_gguf(&s, "qwen3", 8192).unwrap();
        let cpu = Forward::load(&mut r, &s, cfg).unwrap();
        let kind = |w: &Weight| match w.data() {
            crate::weights::Data::F32(_) => "F32",
            crate::weights::Data::Q8_0(_) => "Q8_0",
            crate::weights::Data::Q4K(_) => "Q4_K",
            crate::weights::Data::Q6K(_) => "Q6_K",
        };
        let l = &cpu.layers[0];
        let head = cpu.output.as_ref().unwrap_or(&*cpu.embd);
        eprintln!(
            "{name}: q {} k {} v {} o {} gate {} up {} down {} head {} {}x{}",
            kind(&l.wq),
            kind(&l.wk),
            kind(&l.wv),
            kind(&l.wo),
            kind(&l.gate),
            kind(&l.up),
            kind(&l.down),
            kind(head),
            head.rows,
            head.cols
        );
        let Ok(mut gpu) = GpuForward::load(&cpu) else {
            eprintln!("no GPU; skipped");
            return;
        };
        drop(cpu);
        let prompt: Vec<u32> = (0..64).map(|i| (i * 37 + 5) % 1000).collect();
        gpu.forward(&prompt, 0).unwrap();
        let gen_t = std::time::Instant::now();
        for i in 0..10 {
            gpu.forward(&[7], prompt.len() + i).unwrap();
        }
        let t_fwd = gen_t.elapsed().as_secs_f64() * 100.0;
        let ctx = &gpu.ctx;
        let time = |ds: &[&Dispatch]| {
            let run = || {
                let mut enc = ctx.encoder();
                {
                    let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("llm"), timestamp_writes: None });
                    for d in ds {
                        ctx.run(&mut pass, d);
                    }
                }
                let i = ctx.submit(enc).unwrap();
                ctx.wait(Some(i)).unwrap();
            };
            run();
            let t = std::time::Instant::now();
            for _ in 0..10 {
                run();
            }
            t.elapsed().as_secs_f64() * 100.0
        };
        let all: Vec<&Dispatch> = gpu.token_ops.iter().flatten().chain(&gpu.head).collect();
        let mv: Vec<&Dispatch> = all.iter().copied().filter(|d| d.is_matvec()).collect();
        let rest: Vec<&Dispatch> = all.iter().copied().filter(|d| !d.is_matvec()).collect();
        let head_mv: Vec<&Dispatch> = gpu.head.iter().filter(|d| d.is_matvec()).collect();
        let (t_all, t_mv, t_rest, t_head) = (time(&all), time(&mv), time(&rest), time(&head_mv));
        let gb = gpu.matrices.iter().map(GpuMatrix::bytes).sum::<u64>() as f64 / 1e9;
        eprintln!(
            "{}: {name}: forward() {t_fwd:.1} ms/token; one submission {t_all:.1} ms ({} kernels): matvecs {t_mv:.1} ms ({} kernels, ~{:.0} GB/s over {gb:.2} GB), other kernels {t_rest:.1} ms, output head {t_head:.1} ms",
            gpu.adapter(),
            all.len(),
            mv.len(),
            gb / (t_mv / 1e3)
        );
    }

    /// A prompt of `n` token ids (all inside the synthetic vocabulary of 512).
    fn prompt(n: usize, seed: usize) -> Vec<u32> {
        (0..n).map(|i| ((i * 37 + seed * 101 + 5) % 512) as u32).collect()
    }

    /// A prompt of `n` tokens, then `gen` single tokens, then a 30-token continuation at the
    /// cache's end (a reused prompt prefix), then one more token: the worst relative logit
    /// difference between `a` and `b` (top tokens asserted equal with `top`).
    fn prefill_steps(a: &mut dyn FnMut(&[u32], usize) -> Vec<f32>, b: &mut dyn FnMut(&[u32], usize) -> Vec<f32>, n: usize, what: &str, top: bool) -> f32 {
        let p = prompt(n, n);
        let more = prompt(30, n + 1);
        let mut steps: Vec<(Vec<u32>, usize)> = vec![(p.clone(), 0)];
        steps.extend((0..3).map(|i| (vec![(i * 13 + 2) as u32], n + i)));
        steps.push((more, n + 3));
        steps.push((vec![9], n + 33));
        let mut worst = 0f32;
        for (i, (t, pos)) in steps.iter().enumerate() {
            let (la, lb) = (a(t, *pos), b(t, *pos));
            let r = rel(&lb, &la);
            worst = worst.max(r);
            if top {
                assert_eq!(argmax(&la), argmax(&lb), "{what}: prompt of {n}, step {i}: top token (diff {r})");
            }
        }
        worst
    }

    /// Batched prefill against the CPU (prompts of 1, 7 and 130 tokens: 130 spans two token
    /// tiles of the matmul) and against the GPU's own one-token path, on the all-F32 model (both
    /// sides do the same f32 arithmetic) for both architectures (smollm3 has a NoPE layer, qwen2
    /// biases and halves rope).
    #[test]
    fn batched_prefill_matches_the_cpu_and_the_one_token_path() {
        let Some(ctx) = context() else { return };
        let name = ctx.name().to_string();
        drop(ctx);
        for arch in ["smollm3", "qwen2"] {
            let mut cpu = model_ctx(arch, [F32; 4], 7, false, 256);
            let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
            let mut one = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
            assert!(gpu.batch() >= 130, "batch of {}", gpu.batch());
            one.set_batch(0);
            for n in [1, 7, 130] {
                let what = format!("{arch} F32 on {name}");
                let w = prefill_steps(&mut |t, p| cpu.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
                eprintln!("{what}: prompt of {n}: batched GPU vs CPU {w:.2e}");
                assert!(w <= 1e-4, "{what}, prompt of {n}: {w}");
                let w = prefill_steps(&mut |t, p| one.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
                eprintln!("{what}: prompt of {n}: batched vs one-token GPU {w:.2e}");
                assert!(w <= 1e-4, "{what}, prompt of {n}: batched vs one by one {w}");
            }
        }
    }

    /// Mixed Q4_K / Q6_K / Q8_0 / F32 weights through the batched path (matmul for the K-quants,
    /// a matvec per token for the rest): against the CPU on the dequantized model (the same f32
    /// arithmetic, ≤ 1e-4) and against the CPU on the quantized model within its own
    /// activation-quantization error (as `quantized_model_matches_the_cpu`).
    #[test]
    fn batched_prefill_of_a_quantized_model_matches_the_cpu() {
        let Some(ctx) = context() else { return };
        let name = ctx.name().to_string();
        drop(ctx);
        for (arch, types) in [("smollm3", [Q4_K, Q6_K, Q8_0, F32]), ("qwen2", [Q6_K, Q8_0, Q4_K, F32])] {
            let what = format!("{arch} {types:?} on {name}");
            let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &model_ctx(arch, types, 11, false, 256)).unwrap();
            for n in [1, 7, 130] {
                let mut exact = model_ctx(arch, types, 11, true, 256);
                let w = prefill_steps(&mut |t, p| exact.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
                eprintln!("{what}: prompt of {n}: vs CPU f32 {w:.2e}");
                assert!(w <= 1e-4, "{what}, prompt of {n}: {w}");
                let (mut cpu, mut exact) = (model_ctx(arch, types, 11, false, 256), model_ctx(arch, types, 11, true, 256));
                let cpu_err = prefill_steps(&mut |t, p| exact.forward(t, p).unwrap(), &mut |t, p| cpu.forward(t, p).unwrap(), n, &what, false);
                let mut cpu = model_ctx(arch, types, 11, false, 256);
                let w = prefill_steps(&mut |t, p| cpu.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, false);
                eprintln!("{what}: prompt of {n}: vs CPU quantized {w:.2e} (the CPU's own error {cpu_err:.2e})");
                assert!(w <= 1.5 * cpu_err + 1e-3, "{what}, prompt of {n}: {w} vs the CPU's own {cpu_err}");
            }
        }
    }

    /// Head dimensions the 8-token prefill attention does not take (66: not a multiple of 4;
    /// 512: over 256) run through the batched path on the simple prefill attention kernel and
    /// still match the CPU and the one-token path (F32 weights, both architectures' rope).
    #[test]
    fn batched_prefill_with_other_head_dims_matches_the_cpu() {
        let Some(ctx) = context() else { return };
        drop(ctx);
        for (arch, heads) in [("smollm3", (4, 2, 66)), ("qwen2", (2, 1, 512))] {
            let mut cpu = model_shaped(arch, [F32; 4], 3, false, 256, heads);
            let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
            let mut one = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
            assert!(gpu.batch() >= 130);
            one.set_batch(0);
            for n in [7, 130] {
                let what = format!("{arch} head dim {}", heads.2);
                let w = prefill_steps(&mut |t, p| cpu.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
                assert!(w <= 1e-4, "{what}, prompt of {n}: {w}");
                let w1 = prefill_steps(&mut |t, p| one.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
                assert!(w1 <= 1e-4, "{what}, prompt of {n}: batched vs one by one {w1}");
                eprintln!("{what}: prompt of {n}: vs CPU {w:.2e}, vs one-token GPU {w1:.2e}");
            }
        }
    }

    /// Chunk boundaries: a 130-token prompt in batches of 16 (eight full, one of 2), 7, 2 and
    /// 129 gives the same logits as one batch; a batch never crosses the context.
    #[test]
    fn batched_prefill_chunks_agree() {
        let Some(ctx) = context() else { return };
        let mut cpu = model_ctx("smollm3", [F32; 4], 5, false, 256);
        let mut whole = GpuForward::with_context(ctx, &cpu).unwrap();
        let p = prompt(130, 3);
        let want = whole.forward(&p, 0).unwrap();
        let cpu_want = cpu.forward(&p, 0).unwrap();
        assert!(rel(&want, &cpu_want) <= 1e-4);
        let mut g = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
        for b in [16, 7, 2, 129] {
            g.set_batch(b);
            let got = g.forward(&p, 0).unwrap();
            assert!(rel(&got, &want) <= 1e-5, "batches of {b}: {}", rel(&got, &want));
            // and generation continues from the batched cache
            let (a, c) = (g.forward(&[4], 130).unwrap(), cpu.forward(&[4], 130).unwrap());
            assert!(rel(&a, &c) <= 1e-4, "batches of {b}, next token: {}", rel(&a, &c));
            cpu.truncate(130);
        }
        // fill the context to its last slot in batches, then one more is refused, cache kept
        g.set_batch(64);
        g.forward(&prompt(250, 4), 0).unwrap();
        assert!(g.forward(&prompt(7, 5), 250).is_err(), "past the context of 256");
        assert_eq!(g.len(), 250);
        let a = g.forward(&prompt(6, 5), 250).unwrap();
        cpu.forward(&prompt(250, 4), 0).unwrap();
        let c = cpu.forward(&prompt(6, 5), 250).unwrap();
        assert!(rel(&a, &c) <= 1e-4, "the last slots: {}", rel(&a, &c));
        assert_eq!(g.len(), 256);
        assert!(g.forward(&[1], 256).is_err());
        // a failed device mid-batch is an error and forgets the cache
        g.poison();
        assert!(g.forward(&prompt(9, 6), 0).is_err());
        assert!(g.is_empty());
    }

    #[test]
    fn hostile_calls_are_errors() {
        let Some(ctx) = context() else { return };
        let cpu = model("smollm3", [Q8_0; 4], 3);
        let mut gpu = GpuForward::with_context(ctx, &cpu).unwrap();
        assert!(gpu.forward(&[], 0).is_err());
        assert!(gpu.forward(&[512], 0).is_err(), "token id past the vocabulary");
        assert!(gpu.is_empty(), "a failed call forgets the cache");
        assert!(gpu.forward(&[1], 5).is_err(), "position that skips the cache");
        assert!(gpu.forward(&[1; 65], 0).is_err(), "past the context");
        gpu.forward(&[1, 2], 0).unwrap();
        assert!(gpu.forward(&[512], 2).is_err());
        assert_eq!(gpu.len(), 2, "a refused input keeps the cache");
        assert!(gpu.forward(&[3], 2).is_ok());
        assert_eq!(gpu.len(), 3);
        gpu.truncate(10);
        assert_eq!(gpu.len(), 3);
        // fill the context exactly, then one more is refused
        gpu.forward(&[4; 61], 3).unwrap();
        assert!(gpu.forward(&[4], 64).is_err());
    }

    /// Synthetic timing with SmolLM3-3B's shapes is not possible without its weights; this times
    /// the synthetic model (tiny: dominated by per-kernel overheads). `cargo test --release -p
    /// filmcraft-llm --features gpu --lib gpu_forward::tests::synthetic_speed -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, run by hand"]
    fn synthetic_speed() {
        let Some(ctx) = context() else { return };
        let name = ctx.name().to_string();
        let mut cpu = model("smollm3", [Q4_K, Q6_K, Q4_K, Q6_K], 5);
        let mut gpu = GpuForward::with_context(ctx, &cpu).unwrap();
        let prompt: Vec<u32> = (0..48).map(|i| (i * 37 + 5) % 512).collect();
        let time = |label: &str, f: &mut dyn FnMut(&[u32], usize) -> Result<Vec<f32>, LlmError>| {
            f(&prompt, 0).unwrap();
            let t = std::time::Instant::now();
            f(&prompt, 0).unwrap();
            let pre = prompt.len() as f64 / t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            for i in 0..16 {
                f(&[7], prompt.len() + i).unwrap();
            }
            let gen_rate = 16.0 / t.elapsed().as_secs_f64();
            eprintln!("{name} synthetic 4-layer: {label} prompt {pre:.0} tok/s, generation {gen_rate:.0} tok/s");
        };
        time("GPU", &mut |t, p| gpu.forward(t, p));
        time("CPU", &mut |t, p| cpu.forward(t, p));
    }

    /// Qwen3 (AI4.2): heads wider than embd / heads (q 512 wide on a 256 embedding) and the
    /// per-head q / k norm, text-only. All-F32 weights: the same f32 arithmetic on both sides, so
    /// ≤ 1e-4 of the largest logit (summation order only) with the top token agreeing, through
    /// the one-token path and batched prefill (prompts of 1, 7 and 130 tokens). Mixed
    /// Q4_K / Q6_K / Q8_0 / F32 weights: ≤ 1e-4 against the CPU on the dequantized model and
    /// within the CPU's own activation-quantization error against the quantized CPU (as
    /// `quantized_model_matches_the_cpu`).
    #[test]
    fn qwen3_model_matches_the_cpu() {
        let Some(ctx) = context() else { return };
        let name = ctx.name().to_string();
        drop(ctx);
        let mut cpu = qwen3_model([F32; 4], 7, false, 256);
        assert_eq!((cpu.cfg.embd, cpu.cfg.q_dim, cpu.cfg.head_dim), (256, 512, 128));
        assert!(cpu.cfg.qk_norm && !cpu.cfg.qkv_bias);
        let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
        let what = format!("qwen3 F32 on {name}");
        let worst = compare(&mut cpu, &mut gpu, &what, true);
        assert!(worst <= 1e-4, "{what}: {worst}");
        let mut one = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
        one.set_batch(0);
        for n in [1, 7, 130] {
            let w = prefill_steps(&mut |t, p| cpu.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
            let w1 = prefill_steps(&mut |t, p| one.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
            eprintln!("{what}: prompt of {n}: batched GPU vs CPU {w:.2e}, vs one-token GPU {w1:.2e}");
            assert!(w <= 1e-4 && w1 <= 1e-4, "{what}, prompt of {n}: {w} / {w1}");
        }
        for types in [[Q4_K, Q6_K, Q8_0, F32], [Q8_0, Q4_K, Q4_K, Q6_K]] {
            let what = format!("qwen3 {types:?} on {name}");
            let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &qwen3_model(types, 11, false, 256)).unwrap();
            let mut exact = qwen3_model(types, 11, true, 256);
            let worst = compare(&mut exact, &mut gpu, &format!("{what} vs CPU f32"), true);
            assert!(worst <= 1e-4, "{what}: {worst}");
            let (mut cpu, mut exact) = (qwen3_model(types, 11, false, 256), qwen3_model(types, 11, true, 256));
            let cpu_err = steps_diff(&mut cpu, &mut exact);
            let mut cpu = qwen3_model(types, 11, false, 256);
            let worst = compare(&mut cpu, &mut gpu, &format!("{what} vs CPU quantized (CPU's own quantization error {cpu_err:.2e})"), false);
            assert!(worst <= 1.5 * cpu_err + 1e-3, "{what}: {worst} vs the CPU's own {cpu_err}");
            for n in [7, 130] {
                let mut exact = qwen3_model(types, 11, true, 256);
                let w = prefill_steps(&mut |t, p| exact.forward(t, p).unwrap(), &mut |t, p| gpu.forward(t, p).unwrap(), n, &what, true);
                eprintln!("{what}: prompt of {n}: batched vs CPU f32 {w:.2e}");
                assert!(w <= 1e-4, "{what}, prompt of {n}: {w}");
            }
        }
    }

    /// A multimodal prompt: 3 text tokens, a 4 × 5 picture (20 merged tokens, random encoder rows
    /// and 3 DeepStack taps), 2 text tokens; the position the text after it takes.
    fn picture_prompt(rng: &mut Rng) -> (Vec<MmToken>, Vec<[u32; 3]>, u32, Vec<f32>, Vec<Vec<f32>>) {
        let mut toks: Vec<MmToken> = [5u32, 17, 3].iter().map(|&t| MmToken::Text(t)).collect();
        toks.extend((0..20).map(MmToken::Picture));
        toks.extend([88u32, 42].iter().map(|&t| MmToken::Text(t)));
        let (pos, next) =
            crate::mrope::positions(&[crate::mrope::Span::Text(3), crate::mrope::Span::Picture { rows: 4, cols: 5 }, crate::mrope::Span::Text(2)]).unwrap();
        let rows: Vec<f32> = (0..20 * 256).map(|_| rng.unit() * 0.2).collect();
        let taps: Vec<Vec<f32>> = (0..3).map(|_| (0..20 * 256).map(|_| rng.unit() * 0.1).collect()).collect();
        (toks, pos, next, rows, taps)
    }

    /// M-RoPE sections of the 64 frequency pairs of a 128-wide head (the real model's).
    const SECTIONS: [usize; 3] = [24, 20, 20];

    type MmStep<'a> = dyn FnMut(&[MmToken], &[[u32; 3]], Option<MmPicture<'_>>) -> Vec<f32> + 'a;

    /// The multimodal steps on `a` and `b` (the CPU and a GPU, or two GPUs): the picture prompt,
    /// 4 generated text tokens at the positions after it, then a second picture (2 × 2) after a
    /// text token, continuing the conversation, and one more text token; the worst relative logit
    /// difference, top tokens asserted equal with `top`.
    fn multimodal_steps(a: &mut MmStep<'_>, b: &mut MmStep<'_>, what: &str, top: bool) -> f32 {
        let mut rng = Rng(99);
        let (toks, pos, mut next, rows, taps) = picture_prompt(&mut rng);
        let mut worst = 0f32;
        let mut step = |t: &[MmToken], p: &[[u32; 3]], pic: Option<MmPicture<'_>>, i: usize| {
            let (la, lb) = (a(t, p, pic), b(t, p, pic));
            let r = rel(&lb, &la);
            worst = worst.max(r);
            if top {
                assert_eq!(argmax(&la), argmax(&lb), "{what}: step {i}: top token (diff {r})");
            }
        };
        step(&toks, &pos, Some(MmPicture { tokens: &rows, deepstack: &taps }), 0);
        for (i, t) in [9u32, 300, 511, 0].iter().enumerate() {
            step(&[MmToken::Text(*t)], &[[next; 3]], None, i + 1);
            next += 1;
        }
        let (p2, _) =
            crate::mrope::positions(&[crate::mrope::Span::Text(1), crate::mrope::Span::Picture { rows: 2, cols: 2 }, crate::mrope::Span::Text(1)]).unwrap();
        let pos2: Vec<[u32; 3]> = p2.iter().map(|q| q.map(|v| v + next)).collect();
        let mut toks2 = vec![MmToken::Text(77)];
        toks2.extend((0..4).map(MmToken::Picture));
        toks2.push(MmToken::Text(1));
        let rows2: Vec<f32> = rows[..4 * 256].iter().map(|v| -v).collect();
        let taps2: Vec<Vec<f32>> = taps.iter().map(|t| t[..4 * 256].to_vec()).collect();
        step(&toks2, &pos2, Some(MmPicture { tokens: &rows2, deepstack: &taps2 }), 5);
        step(&[MmToken::Text(2)], &[[next + 3; 3]], None, 6);
        worst
    }

    /// `GpuForward::forward_multimodal` against the CPU's: picture rows as inputs, per-token
    /// M-RoPE angles, DeepStack adds after layers 0–2, the KV cache continuing across calls. The
    /// all-F32 Qwen3 model: ≤ 1e-4 of the largest logit with the top token agreeing (the same f32
    /// arithmetic; the angles are the CPU's own cos / sin values uploaded), through the batched
    /// path (the picture prompt is one batch) and the one-token path (every token, picture tokens
    /// with their DeepStack rows, as its own submission). Mixed quantized weights: ≤ 1e-4 against
    /// the CPU on the dequantized model and within the CPU's own activation-quantization error
    /// against the quantized CPU. A text-only multimodal call equals the plain forward.
    #[test]
    fn multimodal_matches_the_cpu() {
        let Some(ctx) = context() else { return };
        let name = ctx.name().to_string();
        drop(ctx);
        let mut cpu = qwen3_model([F32; 4], 7, false, 256);
        let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
        let mut one = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
        one.set_batch(0);
        let what = format!("qwen3 F32 multimodal on {name}");
        let w = multimodal_steps(
            &mut |t, p, pic| cpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
            &mut |t, p, pic| gpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
            &what,
            true,
        );
        assert_eq!(gpu.len(), cpu.len());
        eprintln!("{what}: batched GPU vs CPU {w:.2e}");
        assert!(w <= 1e-4, "{what}: {w}");
        cpu.reset();
        let w = multimodal_steps(
            &mut |t, p, pic| cpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
            &mut |t, p, pic| one.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
            &what,
            true,
        );
        eprintln!("{what}: one-token GPU vs CPU {w:.2e}");
        assert!(w <= 1e-4, "{what}, token by token: {w}");
        // batches of 7: a picture across a chunk boundary, chunks with and without picture rows,
        // a one-token leftover (review 2026-10-16)
        let mut seven = GpuForward::with_context(GpuContext::new().unwrap(), &cpu).unwrap();
        seven.set_batch(7);
        cpu.reset();
        let w = multimodal_steps(
            &mut |t, p, pic| cpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
            &mut |t, p, pic| seven.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
            &what,
            true,
        );
        eprintln!("{what}: 7-token batches GPU vs CPU {w:.2e}");
        assert!(w <= 1e-4, "{what}, in 7-token batches: {w}");
        // text-only multimodal (positions t = h = w) equals the plain forward on the GPU
        let toks = [5u32, 17, 3, 88];
        gpu.reset();
        let plain = gpu.forward(&toks, 0).unwrap();
        let mm: Vec<MmToken> = toks.iter().map(|&t| MmToken::Text(t)).collect();
        gpu.reset();
        let got = gpu.forward_multimodal(&mm, &[[0; 3], [1; 3], [2; 3], [3; 3]], SECTIONS, None).unwrap();
        assert!(rel(&got, &plain) <= 1e-4, "text-only multimodal vs plain: {}", rel(&got, &plain));
        let next = gpu.forward_multimodal(&[MmToken::Text(42)], &[[4; 3]], SECTIONS, None).unwrap();
        gpu.reset();
        gpu.forward(&toks, 0).unwrap();
        let plain_next = gpu.forward(&[42], 4).unwrap();
        assert!(rel(&next, &plain_next) <= 1e-4);

        for types in [[Q4_K, Q6_K, Q8_0, F32], [Q8_0, Q4_K, Q4_K, Q6_K]] {
            let what = format!("qwen3 {types:?} multimodal on {name}");
            let mut gpu = GpuForward::with_context(GpuContext::new().unwrap(), &qwen3_model(types, 11, false, 256)).unwrap();
            let mut exact = qwen3_model(types, 11, true, 256);
            let w = multimodal_steps(
                &mut |t, p, pic| exact.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
                &mut |t, p, pic| gpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
                &what,
                true,
            );
            eprintln!("{what}: vs CPU f32 {w:.2e}");
            assert!(w <= 1e-4, "{what}: {w}");
            let (mut cpu, mut exact) = (qwen3_model(types, 11, false, 256), qwen3_model(types, 11, true, 256));
            let cpu_err = multimodal_steps(
                &mut |t, p, pic| exact.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
                &mut |t, p, pic| cpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
                &what,
                false,
            );
            let mut cpu = qwen3_model(types, 11, false, 256);
            gpu.reset();
            let w = multimodal_steps(
                &mut |t, p, pic| cpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
                &mut |t, p, pic| gpu.forward_multimodal(t, p, SECTIONS, pic).unwrap(),
                &what,
                false,
            );
            eprintln!("{what}: vs CPU quantized {w:.2e} (the CPU's own error {cpu_err:.2e})");
            assert!(w <= 1.5 * cpu_err + 1e-3, "{what}: {w} vs the CPU's own {cpu_err}");
        }
    }

    /// Hostile multimodal inputs are refused before anything is cached (the cache stays as it
    /// was), and a GPU failure forgets the cache.
    #[test]
    fn hostile_multimodal_calls_are_errors() {
        let Some(ctx) = context() else { return };
        let mut gpu = GpuForward::with_context(ctx, &qwen3_model([Q8_0; 4], 3, false, 64)).unwrap();
        let mut rng = Rng(5);
        let (toks, pos, next, rows, taps) = picture_prompt(&mut rng);
        let pic = MmPicture { tokens: &rows, deepstack: &taps };
        assert!(gpu.forward_multimodal(&toks, &pos, SECTIONS, None).is_err(), "picture token without a picture");
        assert!(gpu.forward_multimodal(&[MmToken::Picture(20)], &pos[..1], SECTIONS, Some(pic)).is_err(), "row past the encoder output");
        assert!(gpu.forward_multimodal(&toks, &pos[..2], SECTIONS, Some(pic)).is_err(), "positions don't match");
        assert!(gpu.forward_multimodal(&toks, &pos, [24, 20, 19], Some(pic)).is_err(), "sections don't fill half a head");
        let short = [vec![1.0f32; 3]];
        assert!(gpu.forward_multimodal(&toks, &pos, SECTIONS, Some(MmPicture { tokens: &rows, deepstack: &short })).is_err(), "a short DeepStack tap");
        assert!(gpu.forward_multimodal(&[], &[], SECTIONS, None).is_err());
        assert!(gpu.forward_multimodal(&[MmToken::Text(512)], &[[0; 3]], SECTIONS, None).is_err(), "token id past the vocabulary");
        assert!(gpu.is_empty(), "nothing cached by refused calls");
        let a = gpu.forward_multimodal(&toks, &pos, SECTIONS, Some(pic)).unwrap();
        assert_eq!(gpu.len(), 25);
        // a refused continuation keeps the cache; a good one continues it
        assert!(gpu.forward_multimodal(&[MmToken::Text(1)], &[[next; 3], [next; 3]], SECTIONS, None).is_err());
        assert_eq!(gpu.len(), 25);
        gpu.forward_multimodal(&[MmToken::Text(1)], &[[next; 3]], SECTIONS, None).unwrap();
        assert_eq!(gpu.len(), 26);
        // the context (64) is not exceeded: 38 more fit, 39 don't
        let many: Vec<MmToken> = (0..39).map(MmToken::Text).collect();
        let many_pos: Vec<[u32; 3]> = (0..39).map(|i| [next + 1 + i; 3]).collect();
        assert!(gpu.forward_multimodal(&many, &many_pos, SECTIONS, None).is_err());
        assert_eq!(gpu.len(), 26);
        gpu.forward_multimodal(&many[..38], &many_pos[..38], SECTIONS, None).unwrap();
        assert_eq!(gpu.len(), 64);
        assert!(gpu.forward_multimodal(&[MmToken::Text(1)], &[[70; 3]], SECTIONS, None).is_err(), "the context is full");
        // after a reset the same prompt gives the same logits
        gpu.reset();
        assert_eq!(gpu.forward_multimodal(&toks, &pos, SECTIONS, Some(pic)).unwrap(), a);
        // a failed device mid-prompt is an error and forgets the cache
        gpu.poison();
        assert!(gpu.forward_multimodal(&toks, &pos, SECTIONS, Some(pic)).is_err());
        assert!(gpu.is_empty());
    }
}
