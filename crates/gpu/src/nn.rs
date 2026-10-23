//! Neural-network building blocks as WGSL compute (fork G3, FORK-PLAN §1a.122): the ops the
//! GPU image encoders are made of. MobileSAM's TinyViT ([`crate::tinyvit`]) is the first user;
//! SAM 2.1's Hiera encoder and memory attention (G3.4) reuse them. Every op takes its shapes
//! at run time, checks them against the buffers it is given (and the device's binding limit)
//! and returns a ready [`NnStep`] (pipeline + bind group + grid) to record into a compute pass,
//! so a whole network is planned once and replayed every frame.
//!
//! The ops (all f32; images are HWC = channel-last unless said otherwise):
//! - [`NnKernels::matmul`]: `o = a·wᵀ + bias` (w `[out][in]` as PyTorch Linear, or `[in][out]`)
//!   with fused GELU / residual epilogues; 64 × 64 tiles in workgroup memory.
//! - [`NnKernels::im2col`]: k × k patches (any stride / padding, CHW or HWC input) so a full
//!   convolution is im2col + matmul.
//! - [`NnKernels::depthwise`]: depthwise k × k convolution (+ bias, optional GELU).
//! - [`NnKernels::layer_norm`]: over channels per row (LayerNorm and LayerNorm2d), optionally
//!   gathering zero-padded attention windows, optionally writing CHW.
//! - [`NnKernels::attention`]: multi-head softmax attention over groups (windows or one global
//!   group), separate query / key-value buffers and counts, any head dim (multiple of 4, ≤ 256),
//!   optional additive bias `[head][query][key]`.
//! - [`NnKernels::add`]: residual add, optionally scattering padded windows back onto tokens.
//! - [`NnKernels::unary`]: GELU (tanh or erf), ReLU, SiLU, sigmoid in place.
//! - [`NnKernels::softmax`]: row softmax in place.
//! - [`NnKernels::resample`]: max / average pooling, nearest / bilinear resizing.
//!
//! Errors are [`NnError`]s (bad shapes, buffers too small or over the binding limit), never
//! panics; GPU validation errors surface through the caller's error scopes.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use wgpu::util::DeviceExt;

/// Workgroups along x before a one-dimensional grid wraps to y.
const STRIDE_X: u32 = 32_768;
/// Largest workgroup count per grid dimension (WebGPU's default limit).
const MAX_GROUPS: u32 = 65_535;
/// Largest attention head dim.
pub const MAX_HEAD_DIM: usize = 256;

/// Why an op could not be set up.
#[derive(Clone, Debug, PartialEq)]
pub struct NnError(pub String);

impl std::fmt::Display for NnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NnError {}

pub type Result<T> = std::result::Result<T, NnError>;

fn err(s: impl Into<String>) -> NnError {
    NnError(s.into())
}

fn u(v: usize) -> Result<u32> {
    u32::try_from(v).map_err(|_| err(format!("size {v} out of range")))
}

fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or_else(|| err("size overflow"))
}

fn prod(v: &[usize]) -> Result<usize> {
    v.iter().try_fold(1usize, |a, b| mul(a, *b))
}

/// A one-dimensional grid of `n` items, `per` per workgroup, wrapped onto y past [`STRIDE_X`].
pub fn groups_1d(n: usize, per: usize) -> Result<[u32; 2]> {
    let g = u(n.div_ceil(per.max(1)))?;
    let gx = g.clamp(1, STRIDE_X);
    let gy = g.div_ceil(gx).max(1);
    if gy > MAX_GROUPS {
        return Err(err("too many workgroups"));
    }
    Ok([gx, gy])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kernel {
    Matmul,
    Im2col,
    Depthwise,
    Norm,
    Add,
    Unary,
    Softmax,
    Resample,
    Bmm,
    Pool2,
    Rope,
    Ew,
}

const KERNELS: [Kernel; 12] = [
    Kernel::Matmul,
    Kernel::Im2col,
    Kernel::Depthwise,
    Kernel::Norm,
    Kernel::Add,
    Kernel::Unary,
    Kernel::Softmax,
    Kernel::Resample,
    Kernel::Bmm,
    Kernel::Pool2,
    Kernel::Rope,
    Kernel::Ew,
];

impl Kernel {
    fn source(self) -> &'static str {
        match self {
            Kernel::Matmul => include_str!("nn/matmul.wgsl"),
            Kernel::Im2col => include_str!("nn/im2col.wgsl"),
            Kernel::Depthwise => include_str!("nn/dwconv.wgsl"),
            Kernel::Norm => include_str!("nn/norm.wgsl"),
            Kernel::Add => include_str!("nn/add.wgsl"),
            Kernel::Unary => include_str!("nn/unary.wgsl"),
            Kernel::Softmax => include_str!("nn/softmax.wgsl"),
            Kernel::Resample => include_str!("nn/resample.wgsl"),
            Kernel::Bmm => include_str!("nn/bmm.wgsl"),
            Kernel::Pool2 => include_str!("nn/pool2.wgsl"),
            Kernel::Rope => include_str!("nn/rope.wgsl"),
            Kernel::Ew => include_str!("nn/ew.wgsl"),
        }
    }
}

const ATTN_SRC: &str = include_str!("nn/attn.wgsl");
const ATTN_HD4_LINE: &str = "const HD4: u32 = 8u;";

/// One planned dispatch: record it with [`NnStep::record`] (any number of times).
pub struct NnStep {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    groups: [u32; 3],
}

impl NnStep {
    pub fn record(&self, pass: &mut wgpu::ComputePass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.dispatch_workgroups(self.groups[0], self.groups[1], self.groups[2]);
    }
}

/// `o[m][n] = a[m][k]·w + bias[n]`, then the epilogue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Matmul {
    pub m: usize,
    pub n: usize,
    pub k: usize,
    /// GELU (tanh form) on the result.
    pub gelu: bool,
    /// Add into what `o` holds (a residual).
    pub accumulate: bool,
    /// GELU (tanh form) after the residual add.
    pub gelu_after: bool,
    /// `w` is `[k][n]` instead of `[n][k]`.
    pub w_kn: bool,
}

/// k × k patches of a `cin`-channel `h` × `w` image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Im2col {
    pub cin: usize,
    pub h: usize,
    pub w: usize,
    pub k: usize,
    pub stride: usize,
    pub pad: usize,
    /// The input is CHW (else HWC).
    pub chw: bool,
}

fn conv_out(size: usize, k: usize, stride: usize, pad: usize) -> Result<usize> {
    let span = pad
        .checked_mul(2)
        .and_then(|p2| size.checked_add(p2))
        .filter(|s| *s >= k && stride > 0 && k > 0)
        .ok_or_else(|| err(format!("convolution: size {size}, kernel {k}, stride {stride}, padding {pad}")))?;
    Ok((span - k) / stride + 1)
}

impl Im2col {
    /// (out_h, out_w): the rows of the patch matrix are out_h · out_w, its columns k·k·cin.
    pub fn out_size(&self) -> Result<(usize, usize)> {
        Ok((conv_out(self.h, self.k, self.stride, self.pad)?, conv_out(self.w, self.k, self.stride, self.pad)?))
    }
}

/// Depthwise k × k convolution of a `c`-channel HWC image; weights `[c][k·k]`, bias `[c]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Depthwise {
    pub c: usize,
    pub h: usize,
    pub w: usize,
    pub k: usize,
    pub stride: usize,
    pub pad: usize,
    pub gelu: bool,
}

impl Depthwise {
    pub fn out_size(&self) -> Result<(usize, usize)> {
        Ok((conv_out(self.h, self.k, self.stride, self.pad)?, conv_out(self.w, self.k, self.stride, self.pad)?))
    }
}

/// An `rh` × `rw` token grid cut into zero-padded `ws` × `ws` windows (window-major rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGrid {
    pub rh: usize,
    pub rw: usize,
    pub ws: usize,
}

impl WindowGrid {
    /// Windows across, windows down.
    pub fn windows(&self) -> (usize, usize) {
        (self.rw.div_ceil(self.ws.max(1)), self.rh.div_ceil(self.ws.max(1)))
    }
    /// Tokens per window.
    pub fn n(&self) -> Result<usize> {
        mul(self.ws, self.ws)
    }
    /// Rows of the padded window layout.
    pub fn rows(&self) -> Result<usize> {
        let (x, y) = self.windows();
        prod(&[x, y, self.n()?])
    }
}

/// Layer norm of `rows` rows of `c` channels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerNorm {
    pub rows: usize,
    pub c: usize,
    pub eps: f32,
    /// Gather the rows of these padded windows (then `rows` must be the grid's rows).
    pub gather: Option<WindowGrid>,
    /// Write `[c][rows]` instead of `[rows][c]`.
    pub channel_major: bool,
}

/// Multi-head attention; offsets and strides in f32 units (multiples of 4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attention {
    /// Windows (or 1 for global attention).
    pub groups: usize,
    /// Queries / keys per group.
    pub nq: usize,
    pub nkv: usize,
    pub heads: usize,
    pub head_dim: usize,
    /// Floats per query row / key-value row / output row.
    pub q_stride: usize,
    pub kv_stride: usize,
    pub out_stride: usize,
    /// Where head 0's query / key / value start in their rows.
    pub q_off: usize,
    pub k_off: usize,
    pub v_off: usize,
    /// Offsets from one head to the next within a query / key-value / output row.
    pub q_head: usize,
    pub kv_head: usize,
    pub out_head: usize,
    pub scale: f32,
}

impl Attention {
    /// Self-attention over a fused qkv row per token (`3 · heads · head_dim`), laid out per
    /// head `[q k v]` (TinyViT) when `per_head`, else `[q…][k…][v…]` (PyTorch's
    /// `reshape(B, N, 3, heads, hd)`, as Hiera / SAM); output rows of `heads · head_dim`.
    pub fn fused_qkv(groups: usize, n: usize, heads: usize, head_dim: usize, per_head: bool) -> Result<Self> {
        let dim = mul(heads, head_dim)?;
        let row = mul(dim, 3)?;
        let (k_off, v_off, head) = if per_head { (head_dim, mul(head_dim, 2)?, mul(head_dim, 3)?) } else { (dim, mul(dim, 2)?, head_dim) };
        Ok(Attention {
            groups,
            nq: n,
            nkv: n,
            heads,
            head_dim,
            q_stride: row,
            kv_stride: row,
            out_stride: dim,
            q_off: 0,
            k_off,
            v_off,
            q_head: head,
            kv_head: head,
            out_head: head_dim,
            scale: 1.0 / (head_dim.max(1) as f32).sqrt(),
        })
    }
}

/// `x += src`: `len` values, or `src` in padded windows scattered onto a `c`-channel grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Add {
    pub len: usize,
    pub scatter: Option<(WindowGrid, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    GeluTanh,
    GeluErf,
    Relu,
    Silu,
    Sigmoid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResampleMode {
    /// k × k windows, stride k.
    MaxPool(usize),
    AvgPool(usize),
    Nearest,
    Bilinear,
}

/// Pool or resize a `c`-channel HWC image from `h` × `w` to `oh` × `ow` (pooling: `oh` =
/// `h / k`, `ow` = `w / k`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resample {
    pub c: usize,
    pub h: usize,
    pub w: usize,
    pub oh: usize,
    pub ow: usize,
    pub mode: ResampleMode,
}

/// One operand of a [`Bmm`]: where batch 0's row 0 starts, the row stride and the batch
/// stride, in floats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Strided {
    pub off: usize,
    pub ld: usize,
    pub bs: usize,
}

impl Strided {
    /// Contiguous rows of `ld` floats.
    pub fn rows(ld: usize) -> Self {
        Strided { off: 0, ld, bs: 0 }
    }
}

/// An activation fused into a [`Bmm`]'s epilogue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Act {
    #[default]
    None,
    GeluTanh,
    GeluErf,
    Relu,
}

/// `o[b][m][n] = act(alpha · Σ_k a[b][m][k] · w[b][n][k] (or w[b][k][n]) + bias[n])`, added into
/// `o` when `accumulate` (after the activation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bmm {
    pub m: usize,
    pub n: usize,
    pub k: usize,
    pub batch: usize,
    pub a: Strided,
    pub w: Strided,
    pub o: Strided,
    pub w_kn: bool,
    pub alpha: f32,
    pub bias: bool,
    pub act: Act,
    pub accumulate: bool,
}

impl Bmm {
    /// A plain linear layer: `m` rows of `k` → `n`, contiguous, PyTorch weight layout, bias.
    pub fn linear(m: usize, n: usize, k: usize) -> Self {
        Bmm {
            m,
            n,
            k,
            batch: 1,
            a: Strided::rows(k),
            w: Strided::rows(k),
            o: Strided::rows(n),
            w_kn: false,
            alpha: 1.0,
            bias: true,
            act: Act::None,
            accumulate: false,
        }
    }
}

/// The ops' pipelines on one device.
pub struct NnKernels {
    device: wgpu::Device,
    pipelines: Vec<wgpu::ComputePipeline>,
    /// Attention pipelines by head dim / 4.
    attn: Mutex<HashMap<u32, wgpu::ComputePipeline>>,
    /// Bound where an op's optional buffer is absent.
    dummy: wgpu::Buffer,
    max_binding: u64,
}

fn pipeline(device: &wgpu::Device, src: &str) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("nn"), source: wgpu::ShaderSource::Wgsl(src.into()) });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("nn"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}

impl NnKernels {
    /// Compiles the kernels (errors surface through the caller's validation scope).
    pub fn new(device: &wgpu::Device) -> Self {
        let pipelines = KERNELS.iter().map(|k| pipeline(device, k.source())).collect();
        let dummy =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("nn-dummy"), contents: &[0; 16], usage: wgpu::BufferUsages::STORAGE });
        let l = device.limits();
        let max_binding = l.max_storage_buffer_binding_size.min(l.max_buffer_size);
        Self { device: device.clone(), pipelines, attn: Mutex::new(HashMap::new()), dummy, max_binding }
    }

    /// The largest buffer an op may bind, in bytes.
    pub fn max_binding(&self) -> u64 {
        self.max_binding
    }

    fn fixed(&self, k: Kernel) -> Result<wgpu::ComputePipeline> {
        KERNELS.iter().position(|x| *x == k).and_then(|i| self.pipelines.get(i)).cloned().ok_or_else(|| err("kernel missing"))
    }

    /// `buf` must hold `n` floats and be bindable.
    fn check(&self, buf: &wgpu::Buffer, n: usize, what: &str) -> Result<()> {
        let need = (n as u64).checked_mul(4).ok_or_else(|| err(format!("{what}: too large")))?;
        if buf.size() < need {
            return Err(err(format!("{what}: needs {need} bytes, the buffer has {}", buf.size())));
        }
        if buf.size() > self.max_binding {
            return Err(err(format!("{what}: {} bytes are over the GPU's {}-byte binding limit", buf.size(), self.max_binding)));
        }
        Ok(())
    }

    fn step(&self, pipeline: wgpu::ComputePipeline, params: &[u32], bufs: &[&wgpu::Buffer], groups: [u32; 2]) -> Result<NnStep> {
        self.step3(pipeline, params, bufs, [groups[0], groups[1], 1])
    }

    fn step3(&self, pipeline: wgpu::ComputePipeline, params: &[u32], bufs: &[&wgpu::Buffer], groups: [u32; 3]) -> Result<NnStep> {
        if groups.iter().any(|g| *g > MAX_GROUPS || *g == 0) {
            return Err(err("too many workgroups"));
        }
        let mut words = params.to_vec();
        words.resize(words.len().div_ceil(4).max(1) * 4, 0);
        let ub = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nn-params"),
            contents: bytemuck::cast_slice(&words),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() }];
        for (i, b) in bufs.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry { binding: u(i + 1)?, resource: b.as_entire_binding() });
        }
        let bind =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("nn"), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        Ok(NnStep { pipeline, bind, groups })
    }

    pub fn matmul(&self, s: &Matmul, a: &wgpu::Buffer, w: &wgpu::Buffer, bias: &wgpu::Buffer, o: &wgpu::Buffer) -> Result<NnStep> {
        if s.m == 0 || s.n == 0 || s.k == 0 {
            return Err(err(format!("matmul {}×{}×{}", s.m, s.n, s.k)));
        }
        self.check(a, mul(s.m, s.k)?, "matmul input")?;
        self.check(w, mul(s.n, s.k)?, "matmul weight")?;
        self.check(bias, s.n, "matmul bias")?;
        self.check(o, mul(s.m, s.n)?, "matmul output")?;
        let flags = u32::from(s.gelu) | u32::from(s.accumulate) << 1 | u32::from(s.gelu_after) << 2 | u32::from(s.w_kn) << 3;
        let groups = [u(s.n.div_ceil(64))?, u(s.m.div_ceil(64))?];
        self.step(self.fixed(Kernel::Matmul)?, &[u(s.m)?, u(s.n)?, u(s.k)?, flags], &[a, w, bias, o], groups)
    }

    pub fn im2col(&self, s: &Im2col, x: &wgpu::Buffer, col: &wgpu::Buffer) -> Result<NnStep> {
        let (oh, ow) = s.out_size()?;
        if s.cin == 0 {
            return Err(err("im2col of no channels"));
        }
        let kdim = prod(&[s.k, s.k, s.cin])?;
        let total = prod(&[oh, ow, kdim])?;
        self.check(x, prod(&[s.cin, s.h, s.w])?, "im2col input")?;
        self.check(col, total, "im2col output")?;
        let (sc, sx, sy) = if s.chw { (mul(s.h, s.w)?, 1, s.w) } else { (1, s.cin, mul(s.w, s.cin)?) };
        let p = [u(total)?, u(s.cin)?, u(s.h)?, u(s.w)?, u(ow)?, u(s.stride)?, u(sc)?, u(sx)?, u(sy)?, u(kdim)?, u(s.k)?, u(s.pad)?];
        self.step(self.fixed(Kernel::Im2col)?, &p, &[x, col], groups_1d(total, 256)?)
    }

    pub fn depthwise(&self, s: &Depthwise, x: &wgpu::Buffer, w: &wgpu::Buffer, bias: &wgpu::Buffer, o: &wgpu::Buffer) -> Result<NnStep> {
        let (oh, ow) = s.out_size()?;
        if s.c == 0 {
            return Err(err("depthwise convolution of no channels"));
        }
        let total = prod(&[oh, ow, s.c])?;
        self.check(x, prod(&[s.h, s.w, s.c])?, "depthwise input")?;
        self.check(w, prod(&[s.c, s.k, s.k])?, "depthwise weight")?;
        self.check(bias, s.c, "depthwise bias")?;
        self.check(o, total, "depthwise output")?;
        let p = [u(total)?, u(s.c)?, u(s.h)?, u(s.w)?, u(ow)?, u(s.stride)?, u32::from(s.gelu), u(s.k)?, u(s.pad)?, 0, 0, 0];
        self.step(self.fixed(Kernel::Depthwise)?, &p, &[x, w, bias, o], groups_1d(total, 256)?)
    }

    pub fn layer_norm(&self, s: &LayerNorm, x: &wgpu::Buffer, gamma: &wgpu::Buffer, beta: &wgpu::Buffer, o: &wgpu::Buffer) -> Result<NnStep> {
        self.layer_norm_with(s, false, x, gamma, beta, o)
    }

    /// [`NnKernels::layer_norm`]; with `zero_pad` a gathered window's padding token is written as
    /// zeros (padding added after the norm, as SAM 2's Hiera) instead of normalising to `beta`.
    pub fn layer_norm_with(
        &self,
        s: &LayerNorm,
        zero_pad: bool,
        x: &wgpu::Buffer,
        gamma: &wgpu::Buffer,
        beta: &wgpu::Buffer,
        o: &wgpu::Buffer,
    ) -> Result<NnStep> {
        if s.rows == 0 || s.c == 0 || !s.eps.is_finite() || s.eps < 0.0 {
            return Err(err(format!("layer norm of {} × {} (eps {})", s.rows, s.c, s.eps)));
        }
        let (mode, g) = match s.gather {
            Some(g) => {
                if g.ws == 0 || g.rows()? != s.rows {
                    return Err(err(format!("layer norm: {} rows for windows of {g:?}", s.rows)));
                }
                self.check(x, prod(&[g.rh, g.rw, s.c])?, "layer norm input")?;
                (1, g)
            }
            None => {
                self.check(x, mul(s.rows, s.c)?, "layer norm input")?;
                (0, WindowGrid { rh: 0, rw: 0, ws: 1 })
            }
        };
        self.check(gamma, s.c, "layer norm weight")?;
        self.check(beta, s.c, "layer norm bias")?;
        self.check(o, mul(s.rows, s.c)?, "layer norm output")?;
        let p = [
            u(s.rows)?,
            u(s.c)?,
            mode,
            u32::from(s.channel_major) | u32::from(zero_pad) << 1,
            u(g.rh)?,
            u(g.rw)?,
            u(g.ws)?,
            u(g.windows().0)?,
            u(g.n()?)?,
            s.eps.to_bits(),
            0,
            0,
        ];
        self.step(self.fixed(Kernel::Norm)?, &p, &[x, gamma, beta, o], groups_1d(s.rows, 1)?)
    }

    /// `bias`: `[heads][nq][nkv]`, or `None`.
    pub fn attention(&self, s: &Attention, q: &wgpu::Buffer, kv: &wgpu::Buffer, bias: Option<&wgpu::Buffer>, o: &wgpu::Buffer) -> Result<NnStep> {
        let quads = [s.head_dim, s.q_stride, s.kv_stride, s.out_stride, s.q_off, s.k_off, s.v_off, s.q_head, s.kv_head, s.out_head];
        if quads.iter().any(|v| !v.is_multiple_of(4)) || s.head_dim == 0 || s.head_dim > MAX_HEAD_DIM {
            return Err(err(format!("attention: head dim {} and every offset / stride must be multiples of 4 (head dim ≤ {MAX_HEAD_DIM})", s.head_dim)));
        }
        if s.groups == 0 || s.nq == 0 || s.nkv == 0 || s.heads == 0 || !s.scale.is_finite() {
            return Err(err(format!("attention: {} groups, {} queries, {} keys, {} heads", s.groups, s.nq, s.nkv, s.heads)));
        }
        let last = |rows: usize, stride: usize, off: usize, head: usize| -> Result<usize> {
            let r = mul(rows - 1, stride)?;
            r.checked_add(off)
                .and_then(|v| v.checked_add((s.heads - 1).checked_mul(head)?))
                .and_then(|v| v.checked_add(s.head_dim))
                .ok_or_else(|| err("attention: size overflow"))
        };
        let qrows = mul(s.groups, s.nq)?;
        let kvrows = mul(s.groups, s.nkv)?;
        self.check(q, last(qrows, s.q_stride, s.q_off, s.q_head)?, "attention queries")?;
        self.check(kv, last(kvrows, s.kv_stride, s.k_off.max(s.v_off), s.kv_head)?, "attention keys / values")?;
        self.check(o, last(qrows, s.out_stride, 0, s.out_head)?, "attention output")?;
        let bias_buf = match bias {
            Some(b) => {
                self.check(b, prod(&[s.heads, s.nq, s.nkv])?, "attention bias")?;
                b
            }
            None => &self.dummy,
        };
        let total = prod(&[s.groups, s.heads, s.nq])?;
        let q4 = |v: usize| u(v / 4);
        let p = [
            u(total)?,
            u(s.nq)?,
            u(s.nkv)?,
            u(s.heads)?,
            q4(s.q_stride)?,
            q4(s.kv_stride)?,
            q4(s.q_off)?,
            q4(s.k_off)?,
            q4(s.v_off)?,
            q4(s.q_head)?,
            q4(s.kv_head)?,
            q4(s.out_stride)?,
            q4(s.out_head)?,
            u32::from(bias.is_some()),
            s.scale.to_bits(),
            0,
        ];
        let hd4 = u(s.head_dim / 4)?;
        let pipe = {
            let mut cache = self.attn.lock().unwrap_or_else(PoisonError::into_inner);
            cache.entry(hd4).or_insert_with(|| pipeline(&self.device, &ATTN_SRC.replace(ATTN_HD4_LINE, &format!("const HD4: u32 = {hd4}u;")))).clone()
        };
        self.step(pipe, &p, &[q, kv, bias_buf, o], groups_1d(total, 64)?)
    }

    pub fn add(&self, s: &Add, src: &wgpu::Buffer, x: &wgpu::Buffer) -> Result<NnStep> {
        if s.len == 0 {
            return Err(err("add of nothing"));
        }
        self.check(x, s.len, "add target")?;
        let p = match s.scatter {
            Some((g, c)) => {
                if g.ws == 0 || c == 0 || prod(&[g.rh, g.rw, c])? != s.len {
                    return Err(err(format!("add: {} values for windows of {g:?} × {c}", s.len)));
                }
                self.check(src, mul(g.rows()?, c)?, "add source")?;
                [u(s.len)?, u(c)?, u(g.rh)?, u(g.rw)?, u(g.ws)?, u(g.windows().0)?, u(g.n()?)?, 1]
            }
            None => {
                self.check(src, s.len, "add source")?;
                [u(s.len)?, 1, 0, 0, 1, 1, 1, 0]
            }
        };
        self.step(self.fixed(Kernel::Add)?, &p, &[src, x], groups_1d(s.len, 256)?)
    }

    pub fn unary(&self, op: UnaryOp, len: usize, x: &wgpu::Buffer) -> Result<NnStep> {
        self.check(x, len, "activation")?;
        let code = match op {
            UnaryOp::GeluTanh => 0,
            UnaryOp::GeluErf => 1,
            UnaryOp::Relu => 2,
            UnaryOp::Silu => 3,
            UnaryOp::Sigmoid => 4,
        };
        self.step(self.fixed(Kernel::Unary)?, &[u(len)?, code, 0, 0], &[x], groups_1d(len, 256)?)
    }

    pub fn softmax(&self, rows: usize, n: usize, x: &wgpu::Buffer) -> Result<NnStep> {
        if rows == 0 || n == 0 {
            return Err(err("softmax of nothing"));
        }
        self.check(x, mul(rows, n)?, "softmax")?;
        self.step(self.fixed(Kernel::Softmax)?, &[u(rows)?, u(n)?, 0, 0], &[x], groups_1d(rows, 1)?)
    }

    pub fn resample(&self, s: &Resample, x: &wgpu::Buffer, o: &wgpu::Buffer) -> Result<NnStep> {
        if s.c == 0 || s.h == 0 || s.w == 0 || s.oh == 0 || s.ow == 0 {
            return Err(err(format!("resample {s:?}")));
        }
        let (mode, k) = match s.mode {
            ResampleMode::MaxPool(k) | ResampleMode::AvgPool(k) => {
                if k == 0 || s.oh != s.h / k || s.ow != s.w / k {
                    return Err(err(format!("pooling {s:?}")));
                }
                (if matches!(s.mode, ResampleMode::MaxPool(_)) { 0 } else { 1 }, k)
            }
            ResampleMode::Nearest => (2, 1),
            ResampleMode::Bilinear => (3, 1),
        };
        let total = prod(&[s.oh, s.ow, s.c])?;
        self.check(x, prod(&[s.h, s.w, s.c])?, "resample input")?;
        self.check(o, total, "resample output")?;
        let p = [u(total)?, u(s.c)?, u(s.h)?, u(s.w)?, u(s.oh)?, u(s.ow)?, mode, u(k)?];
        self.step(self.fixed(Kernel::Resample)?, &p, &[x, o], groups_1d(total, 256)?)
    }

    /// Strided, batched matmul (see [`Bmm`]); `bias` is `[n]` (with `s.bias`).
    pub fn bmm(&self, s: &Bmm, a: &wgpu::Buffer, w: &wgpu::Buffer, bias: Option<&wgpu::Buffer>, o: &wgpu::Buffer) -> Result<NnStep> {
        if s.m == 0 || s.n == 0 || s.k == 0 || s.batch == 0 || !s.alpha.is_finite() {
            return Err(err(format!("bmm {}×{}×{} ×{}", s.m, s.n, s.k, s.batch)));
        }
        // the last float each operand touches
        let last = |st: &Strided, rows: usize, cols: usize| -> Result<usize> {
            let v = mul(s.batch - 1, st.bs)?
                .checked_add(mul(rows - 1, st.ld)?)
                .and_then(|v| v.checked_add(cols - 1))
                .and_then(|v| v.checked_add(st.off))
                .ok_or_else(|| err("bmm: size overflow"))?;
            if v >= u32::MAX as usize {
                return Err(err("bmm: an index is over 2³²"));
            }
            Ok(v + 1)
        };
        self.check(a, last(&s.a, s.m, s.k)?, "bmm input")?;
        let wl = if s.w_kn { last(&s.w, s.k, s.n)? } else { last(&s.w, s.n, s.k)? };
        self.check(w, wl, "bmm weight")?;
        self.check(o, last(&s.o, s.m, s.n)?, "bmm output")?;
        let bias_buf = match (s.bias, bias) {
            (true, Some(b)) => {
                self.check(b, s.n, "bmm bias")?;
                b
            }
            (true, None) => return Err(err("bmm: bias asked for but not given")),
            (false, _) => &self.dummy,
        };
        let act = match s.act {
            Act::None => 0,
            Act::GeluTanh => 1,
            Act::GeluErf => 16,
            Act::Relu => 32,
        };
        let flags = act | u32::from(s.accumulate) << 1 | u32::from(s.w_kn) << 3 | u32::from(s.bias) << 6;
        let p = [
            u(s.m)?,
            u(s.n)?,
            u(s.k)?,
            flags,
            u(s.a.off)?,
            u(s.a.ld)?,
            u(s.a.bs)?,
            u(s.w.off)?,
            u(s.w.ld)?,
            u(s.w.bs)?,
            u(s.o.off)?,
            u(s.o.ld)?,
            u(s.o.bs)?,
            s.alpha.to_bits(),
            0,
            0,
        ];
        let groups = [u(s.n.div_ceil(64))?, u(s.m.div_ceil(64))?, u(s.batch)?];
        self.step3(self.fixed(Kernel::Bmm)?, &p, &[a, w, bias_buf, o], groups)
    }

    /// 2 × 2 max pooling of `batch` grids of `h` × `w` tokens read with a row `stride` and a
    /// channel offset `off` (`c` channels) into a contiguous `[batch][h/2][w/2][c]`.
    #[allow(clippy::too_many_arguments)]
    pub fn pool2(&self, batch: usize, h: usize, w: usize, c: usize, stride: usize, off: usize, x: &wgpu::Buffer, o: &wgpu::Buffer) -> Result<NnStep> {
        if batch == 0 || h < 2 || w < 2 || c == 0 || off.checked_add(c).is_none_or(|e| e > stride) {
            return Err(err(format!("pool2: {batch} × {h} × {w} × {c} (stride {stride}, offset {off})")));
        }
        let total = prod(&[batch, h / 2, w / 2, c])?;
        self.check(x, prod(&[batch, h, w, stride])?, "pool2 input")?;
        self.check(o, total, "pool2 output")?;
        let p = [u(total)?, u(c)?, u(h)?, u(w)?, u(stride)?, u(off)?, 0, 0];
        self.step(self.fixed(Kernel::Pool2)?, &p, &[x, o], groups_1d(total, 256)?)
    }

    /// RoPE in place on `rows` rows of `x` (row stride `stride`, `2 · pairs` values from `off`),
    /// tables `[period][pairs]`.
    #[allow(clippy::too_many_arguments)]
    pub fn rope(
        &self,
        rows: usize,
        pairs: usize,
        stride: usize,
        off: usize,
        period: usize,
        x: &wgpu::Buffer,
        cos: &wgpu::Buffer,
        sin: &wgpu::Buffer,
    ) -> Result<NnStep> {
        if rows == 0 || pairs == 0 || period == 0 || mul(pairs, 2)?.checked_add(off).is_none_or(|e| e > stride) {
            return Err(err(format!("rope: {rows} rows of {pairs} pairs (stride {stride}, offset {off}, period {period})")));
        }
        let total = mul(rows, pairs)?;
        self.check(x, mul(rows, stride)?, "rope input")?;
        self.check(cos, mul(period, pairs)?, "rope cos table")?;
        self.check(sin, mul(period, pairs)?, "rope sin table")?;
        let p = [u(total)?, u(pairs)?, u(stride)?, u(off)?, u(period)?, 0, 0, 0];
        self.step(self.fixed(Kernel::Rope)?, &p, &[x, cos, sin], groups_1d(total, 256)?)
    }

    /// `o = alpha · a + beta · b` (`b` repeats every `blen` values).
    #[allow(clippy::too_many_arguments)]
    pub fn ew(&self, len: usize, blen: usize, alpha: f32, beta: f32, a: &wgpu::Buffer, b: &wgpu::Buffer, o: &wgpu::Buffer) -> Result<NnStep> {
        if len == 0 || blen == 0 || !alpha.is_finite() || !beta.is_finite() {
            return Err(err(format!("ew of {len} / {blen}")));
        }
        self.check(a, len, "ew input")?;
        self.check(b, blen, "ew second input")?;
        self.check(o, len, "ew output")?;
        let p = [u(len)?, u(blen)?, alpha.to_bits(), beta.to_bits()];
        self.step(self.fixed(Kernel::Ew)?, &p, &[a, b, o], groups_1d(len, 256)?)
    }
}

#[cfg(test)]
#[path = "nn_tests.rs"]
mod tests;
