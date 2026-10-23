//! Colour-operation chains on the GPU (fork ADR 0003): a [`ColorOp`] list becomes a flat `f32`
//! program that `colorops.wgsl` runs per pixel in one compute pass (the FX stage's
//! [`FxOp::ColorChain`](filmcraft_render::gpufx::FxOp::ColorChain) step).

use filmcraft_color::ops::{ColorOp, Tf};

const CO_MATRIX: f32 = 1.0;
const CO_DECODE: f32 = 2.0;
const CO_ENCODE: f32 = 3.0;
const CO_TABLE: f32 = 4.0;
const CO_LGGO: f32 = 5.0;
const CO_SATURATION: f32 = 6.0;
const CO_HLG_OOTF: f32 = 7.0;
const CO_TONE_MAP: f32 = 8.0;
const CO_GAMUT_COMPRESS: f32 = 9.0;
const CO_DESATURATE: f32 = 10.0;
const CO_CLAMP: f32 = 11.0;
const CO_TONE_REGIONS: f32 = 12.0;
const CO_CONTRAST: f32 = 13.0;
const CO_SPECULAR: f32 = 14.0;
const CO_SPLIT_TONE: f32 = 15.0;
const CO_VIGNETTE: f32 = 16.0;
const CO_CURVE: f32 = 17.0;
const CO_WHEELS: f32 = 18.0;
const CO_HSL_CURVES: f32 = 19.0;
const CO_LUT: f32 = 20.0;
const CO_LOOK: f32 = 21.0;
const CO_HSL_KEY: f32 = 22.0;
const CO_HSL_CORRECT: f32 = 23.0;
const CO_GRAIN: f32 = 24.0;
const CO_STASH: f32 = 25.0;
const CO_MIX_STASH: f32 = 26.0;
/// GPU-only codes (no [`ColorOp`]): an HSL Secondary's key and correct in one chain (`fx.rs`
/// `fused_steps`; `colorops.wgsl` says what they do).
pub(crate) const CO_KEY_BEGIN: f32 = 27.0;
pub(crate) const CO_KEY_END: f32 = 28.0;
const CO_WARP: f32 = 29.0;
/// HDR zone wheels (fork G4.2; 29 is the colour warper's, G4.1).
const CO_ZONE_WHEELS: f32 = 30.0;
/// The 3D qualifier's key and despill (fork G4.3; one code, a sub-op: `colorops.wgsl`).
const CO_KEY3D: f32 = 31.0;

/// Values a program may hold (tables of a few thousand samples each; a chain is a handful of ops).
pub const MAX_PROGRAM: usize = filmcraft_color::ops::MAX_GPU_PROGRAM;

fn tf(t: Tf) -> f32 {
    t.code() as f32
}

/// A table as `lo, hi, n, values…` (None: too long to count in `f32`).
fn table(p: &mut Vec<f32>, t: &filmcraft_color::ops::Table) -> Option<()> {
    let n = t.values.len();
    if n >= 1 << 24 {
        return None;
    }
    p.extend([t.lo, t.hi, n as f32]);
    p.extend(t.values.iter());
    Some(())
}

fn flag(b: bool) -> f32 {
    f32::from(u8::from(b))
}

/// A LUT as `n1, dmin, dmax, shaper…, size, dmin, dmax, cube…` (0 sizes: absent). None: too
/// large to index in `f32`.
fn lut(p: &mut Vec<f32>, l: &filmcraft_color::Lut) -> Option<()> {
    match &l.shaper {
        Some(s) if s.data.len() < 1 << 22 => {
            p.push(s.data.len() as f32);
            p.extend(s.domain_min.iter().chain(&s.domain_max));
            p.extend(s.data.iter().flatten());
        }
        Some(_) => return None,
        None => p.extend([0.0; 7]),
    }
    match &l.cube {
        Some(c) if c.size < 1 << 8 => {
            p.push(c.size as f32);
            p.extend(c.domain_min.iter().chain(&c.domain_max));
            p.extend(c.data.iter().flatten());
        }
        Some(_) => return None,
        None => p.extend([0.0; 7]),
    }
    Some(())
}

/// HDR zone wheels as `code, weights, pivot, global (exposure, sat, shift), zones (edge, falloff,
/// exposure, sat, shift)…` ([`filmcraft_color::ops::ZONE_PROGRAM_LEN`] values; the pivot as
/// [`ZoneWheels::stops`](filmcraft_color::ops::ZoneWheels::stops) reads it).
fn zone_wheels(p: &mut Vec<f32>, z: &filmcraft_color::ops::ZoneWheels) {
    let pivot = if z.pivot.is_finite() && z.pivot > 0.0 { z.pivot } else { 0.18 };
    p.push(CO_ZONE_WHEELS);
    p.extend(z.weights);
    p.push(pivot);
    p.extend([z.global.exposure, z.global.sat]);
    p.extend(z.global.shift);
    for zone in &z.zones {
        p.extend([zone.edge, zone.falloff, zone.adjust.exposure, zone.adjust.sat]);
        p.extend(zone.adjust.shift);
    }
}

/// The program of a chain (None: too large, or a table too long to index in `f32`).
pub fn program(ops: &[ColorOp]) -> Option<Vec<f32>> {
    program_and_codes(ops).map(|(p, _)| p)
}

/// The program of a chain and the code of each op in it, in order (what [`specialise`] bakes in).
pub fn program_and_codes(ops: &[ColorOp]) -> Option<(Vec<f32>, Vec<u32>)> {
    let mut p = Vec::new();
    let mut codes = Vec::with_capacity(ops.len());
    for op in ops {
        let start = p.len();
        match op {
            ColorOp::Matrix { m, offset } => {
                p.push(CO_MATRIX);
                p.extend(m.iter().flatten());
                p.extend(offset);
            }
            ColorOp::Decode(t) => p.extend([CO_DECODE, tf(*t)]),
            ColorOp::Encode(t) => p.extend([CO_ENCODE, tf(*t)]),
            ColorOp::Table(t) => {
                p.push(CO_TABLE);
                table(&mut p, t)?;
            }
            ColorOp::Lggo { lift, gamma, gain, offset } => {
                p.push(CO_LGGO);
                for v in [lift, gamma, gain, offset] {
                    p.extend(v);
                }
            }
            ColorOp::Saturation { sat, vibrance, weights } => {
                p.extend([CO_SATURATION, *sat, *vibrance]);
                p.extend(weights);
            }
            ColorOp::HlgOotf { weights, k, exponent } => {
                p.push(CO_HLG_OOTF);
                p.extend(weights);
                p.extend([*k, *exponent]);
            }
            ColorOp::ToneMap { src_peak, dst_peak, ratio } => {
                p.extend([CO_TONE_MAP, *src_peak, *dst_peak]);
                table(&mut p, ratio)?;
            }
            ColorOp::GamutCompress => p.push(CO_GAMUT_COMPRESS),
            ColorOp::DesaturateIntoGamut { weights } => {
                p.push(CO_DESATURATE);
                p.extend(weights);
            }
            ColorOp::Clamp { lo, hi } => p.extend([CO_CLAMP, *lo, *hi]),
            ColorOp::ToneRegions { shadows, highlights, weights } => {
                p.extend([CO_TONE_REGIONS, *shadows, *highlights]);
                p.extend(weights);
            }
            ColorOp::Contrast { amount, keep_above_one } => p.extend([CO_CONTRAST, *amount, f32::from(u8::from(*keep_above_one))]),
            ColorOp::Specular { gain } => p.extend([CO_SPECULAR, *gain]),
            ColorOp::SplitTone { shadows, highlights, weights } => {
                p.push(CO_SPLIT_TONE);
                for v in [shadows, highlights, weights] {
                    p.extend(v);
                }
            }
            ColorOp::Vignette { amount, midpoint, feather, sx, size } => {
                p.extend([CO_VIGNETTE, *amount, *midpoint, *feather, *sx]);
                p.extend(size);
            }
            ColorOp::Curve { table: t, channels, hdr } => {
                p.extend([CO_CURVE, flag(*hdr), flag(channels[0]), flag(channels[1]), flag(channels[2])]);
                table(&mut p, t)?;
            }
            ColorOp::Wheels { shadows, midtones, highlights, weights } => {
                p.push(CO_WHEELS);
                for v in [shadows, midtones, highlights, weights] {
                    p.extend(v);
                }
            }
            ColorOp::HslCurves(h) => {
                p.extend([CO_HSL_CURVES, flag(h.hdr)]);
                for t in [&h.hue_vs_hue, &h.hue_vs_sat, &h.hue_vs_luma, &h.luma_vs_sat, &h.sat_vs_sat] {
                    match t {
                        Some(t) => table(&mut p, t)?,
                        None => p.extend([0.0; 3]),
                    }
                }
            }
            ColorOp::Lut { lut: l, intensity, hdr } => {
                p.extend([CO_LUT, *intensity, flag(*hdr)]);
                lut(&mut p, l)?;
            }
            ColorOp::HslKey { hue, range, sat_min, sat_max, luma_min, luma_max, soft, hdr } => {
                p.extend([CO_HSL_KEY, *hue, *range, *sat_min, *sat_max, *luma_min, *luma_max, *soft, flag(*hdr)]);
            }
            ColorOp::Key3d(k) => {
                // `code, 0, hdr, soft, ni, ne, (x, y, z, r)…` (≤ 64 a side: exact in f32)
                let (inc, exc) = (k.include(), k.exclude());
                p.extend([CO_KEY3D, 0.0, flag(k.hdr()), k.soft(), inc.len() as f32, exc.len() as f32]);
                p.extend(inc.iter().chain(exc).flatten());
            }
            ColorOp::Despill { dir, amount } => p.extend([CO_KEY3D, 1.0, dir[0], dir[1], dir[2], *amount]),
            ColorOp::Stash => p.push(CO_STASH),
            ColorOp::MixStash { invert } => p.extend([CO_MIX_STASH, flag(*invert)]),
            ColorOp::HslCorrect { hue_shift, sat, temp, tint, show_mask, hdr } => {
                p.extend([CO_HSL_CORRECT, *hue_shift, *sat, *temp, *tint, (*show_mask).min(16) as f32, flag(*hdr)]);
            }
            ColorOp::Warp2D(w) => {
                // `code, space, preserve luma, weights, cols, rows, (du, dv)…` (sides ≤ 1024: exact in f32)
                p.extend([CO_WARP, w.space().code() as f32, flag(w.preserve_luma())]);
                p.extend(w.weights());
                p.extend([w.cols() as f32, w.rows() as f32]);
                p.extend(w.field());
            }
            ColorOp::Look { look, intensity, hdr } => p.extend([CO_LOOK, (*look).min(1 << 20) as f32, *intensity, flag(*hdr)]),
            ColorOp::Grain { strength, inv_size, softness, seed, weights } => {
                // (the seed as two exact 16-bit halves)
                p.extend([CO_GRAIN, *strength, *inv_size, *softness, (seed & 0xffff) as f32, (seed >> 16) as f32]);
                p.extend(weights);
            }
            ColorOp::ZoneWheels(z) => zone_wheels(&mut p, z),
        }
        // (every op's first value is its code)
        codes.push(*p.get(start)? as u32);
        if p.len() > MAX_PROGRAM {
            return None;
        }
    }
    Some((p, codes))
}

/// Chains longer than this run on the interpreter (one shader per op sequence is compiled and
/// cached; cold compiles grow with length: 19 ops ~45 ms on the RX 6700 XT / ~110 ms on the UHD
/// 750, 128 ops 0.5 / 2.4 s).
pub const MAX_SPECIALISED_OPS: usize = 64;

/// `colorops.wgsl` with its interpreter (`run_chain`, between the `RUN_CHAIN_BEGIN` /
/// `RUN_CHAIN_END` markers) replaced by straight-line code for a chain of these op codes: one
/// function per distinct code, made from that code's `case` in `chain_op`, called once per op in
/// order (fork U0.2.3b: the interpreter's switch costs Intel's iGPU ~50 ms per op at 6.2K; inlining
/// the whole switch per op instead made cold compiles grow to seconds). Same arithmetic, same
/// program buffer. None: no codes, too many, or a code / marker / case not found in `src`.
pub fn specialise(src: &str, codes: &[u32]) -> Option<String> {
    const BEGIN: &str = "// RUN_CHAIN_BEGIN";
    const END: &str = "// RUN_CHAIN_END";
    if codes.is_empty() || codes.len() > MAX_SPECIALISED_OPS {
        return None;
    }
    let (begin, end) = (src.find(BEGIN)?, src.find(END)?);
    if end < begin {
        return None;
    }
    let (head, tail) = (src.get(..begin)?, src.get(end..)?);
    let interp = src.get(src.find("fn chain_op(")?..begin)?;
    let mut distinct: Vec<u32> = codes.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    let mut f = String::new();
    for code in &distinct {
        let body = case_body(interp, code_name(src, *code)?)?;
        f.push_str(&format!(
            "fn chain_op_{code}(c_in: vec3<f32>, xy: vec2<f32>, pcp: ptr<function, u32>, len: u32) -> vec3<f32> {{\n    var c = c_in;\n    var pc = *pcp;\n    {{{body}}}\n    *pcp = pc;\n    return c;\n}}\n\n"
        ));
    }
    f.push_str(
        "fn run_chain(c0: vec3<f32>, xy: vec2<f32>) -> vec3<f32> {\n    var c = c0;\n    stash_reg = c0;\n    var pc: u32 = 0u;\n    let len = min(u.i1.x, arrayLength(&prog));\n",
    );
    for code in codes {
        f.push_str(&format!("    c = chain_op_{code}(c, xy, &pc, len);\n"));
    }
    f.push_str("    return c;\n}\n");
    Some(format!("{head}{f}{tail}"))
}

/// A pipeline running `entry` of `src` (the chain shader for some format) specialised to `codes`
/// ([`specialise`]). None: not specialisable, or the build failed; a failure is reported here (the
/// caller runs the interpreter), not as an uncaptured error that would end GPU rendering.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn build_specialised(device: &wgpu::Device, layout: &wgpu::PipelineLayout, src: &str, codes: &[u32], entry: &str) -> Option<wgpu::ComputePipeline> {
    let src = specialise(src, codes)?;
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device
        .create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("filmcraft-colorops-specialised"), source: wgpu::ShaderSource::Wgsl(src.into()) });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("colorops-specialised"),
        layout: Some(layout),
        module: &module,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    });
    if let Some(e) = pollster::block_on(scope.pop()) {
        log::warn!("specialised colour chain ({} ops) failed to build, using the interpreter: {e}", codes.len());
        return None;
    }
    Some(pipeline)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn build_specialised(
    _device: &wgpu::Device,
    _layout: &wgpu::PipelineLayout,
    _src: &str,
    _codes: &[u32],
    _entry: &str,
) -> Option<wgpu::ComputePipeline> {
    None
}

/// The name of the `const CO_…: u32 = <code>u;` in `src`.
fn code_name(src: &str, code: u32) -> Option<&str> {
    src.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("const ")?;
        let (name, value) = rest.split_once(": u32 = ")?;
        (name.starts_with("CO_") && value.strip_suffix("u;")?.parse::<u32>().ok()? == code).then_some(name)
    })
}

/// The text inside the braces of `case <name>: { … }` in `src` (braces in `//` comments ignored).
fn case_body<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let open = src.find(&format!("case {name}: {{"))? + "case : {".len() + name.len();
    let mut depth = 1u32;
    let mut comment = false;
    let bytes = src.as_bytes();
    let mut i = open;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\n' => comment = false,
            b'/' if !comment && bytes.get(i + 1) == Some(&b'/') => comment = true,
            b'{' if !comment => depth += 1,
            b'}' if !comment => {
                depth -= 1;
                if depth == 0 {
                    return src.get(open..i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Format of the compositor's accumulator, which the output pass reads and writes.
const OUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The output transform of a colour-managed sequence on the composited picture
/// ([`OutputStage`](filmcraft_render::plan::OutputStage)): the chain shader, built for the
/// accumulator's format, run from the accumulator into a texture of the same format.
pub(crate) struct OutputPass {
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    /// The shader source and layout for chains specialised to their op codes, and those
    /// pipelines by op codes (None: failed to build; the interpreter runs that chain).
    src: String,
    pl: wgpu::PipelineLayout,
    special: std::sync::Mutex<std::collections::HashMap<Vec<u32>, Option<wgpu::ComputePipeline>>>,
}

/// Output-pass pipelines kept at most (one per colour-management setup in use; cleared when full).
const MAX_OUTPUT_PIPELINES: usize = 8;

impl OutputPass {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let src = include_str!("colorops.wgsl").replace("rgba32float", "rgba16float");
        let shader = device
            .create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("filmcraft-output"), source: wgpu::ShaderSource::Wgsl(src.as_str().into()) });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("output"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: OUT_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("output"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("output"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("chain_px"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, bgl, src, pl, special: Default::default() }
    }

    /// The pipeline for a chain of `codes`: specialised (built on first use, cached) unless that
    /// is off or fails.
    fn pipeline_for(&self, device: &wgpu::Device, codes: &[u32]) -> Option<wgpu::ComputePipeline> {
        if codes.is_empty() || codes.len() > MAX_SPECIALISED_OPS || !crate::fx::specialise_chains() {
            return None;
        }
        let mut cache = self.special.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(p) = cache.get(codes) {
            return p.clone();
        }
        let built = build_specialised(device, &self.pl, &self.src, codes, "chain_px");
        if cache.len() >= MAX_OUTPUT_PIPELINES {
            cache.clear();
        }
        cache.insert(codes.to_vec(), built.clone());
        built
    }

    /// Record `ops` from `src` into `dst` (both `w`×`h`, the accumulator's format). None: the
    /// program does not fit (the picture stays unconverted).
    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        (w, h): (u32, u32),
        ops: &[ColorOp],
    ) -> Option<()> {
        let (prog, codes) = program_and_codes(ops)?;
        let special = self.pipeline_for(device, &codes);
        let mut u = Vec::with_capacity(96);
        for v in [0u32, w, h, 0, prog.len() as u32, 0, 0, 0] {
            u.extend_from_slice(&v.to_le_bytes());
        }
        u.resize(96, 0);
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("output-u"),
            size: u.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ubuf, 0, &u);
        let pbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("output-program"),
            size: (prog.len().max(4) * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bytes: Vec<u8> = prog.iter().flat_map(|v| v.to_le_bytes()).collect();
        queue.write_buffer(&pbuf, 0, &bytes);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("output"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(dst) },
                wgpu::BindGroupEntry { binding: 4, resource: pbuf.as_entire_binding() },
            ],
        });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("output"), timestamp_writes: None });
        pass.set_pipeline(special.as_ref().unwrap_or(&self.pipeline));
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
        Some(())
    }
}
