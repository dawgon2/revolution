//! Projective warps of comp programs on the GPU (fork C3.8, `comp_warp.wgsl`): Tracker / Planar
//! Tracker corner pins and planar match moves ([`Pass::Warp`]).
//!
//! The CPU oracle is [`warp_projective`]: each output pixel centre mapped through the inverse
//! homography, a bilinear sample with a transparent border, nothing where the map has no finite
//! value or lands more than a pixel outside the source; a map whose area scale at the source
//! centre is below 1/4 first halves the source (2 × 2 box, repeatedly). [`levels`] makes that
//! choice in f64 exactly as the CPU does, the GPU then runs the halvings (`Rgba32Float` levels,
//! kept between frames) and one full-screen draw sampling the last level by hand (no hardware
//! filter: the CPU's border and rounding rules are reproduced, perspective-correct per pixel).
//!
//! [`Pass::Warp`]: filmcraft_render::comp::Pass::Warp
//! [`warp_projective`]: filmcraft_render::comp::track::warp_projective

use filmcraft_geom::Vec2;
use filmcraft_render::track::Homography;

/// Format of the halved source levels (f32: the CPU halves in f32).
pub(crate) const LEVEL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// Bytes per level texel.
pub(crate) const LEVEL_TEXEL_BYTES: u64 = 16;
/// Most halvings (a 16K source reaches 4 × 4 within 13; the CPU stops below 4 pixels).
const MAX_LEVELS: usize = 24;

/// The two pipelines and their layout.
pub(crate) struct WarpKernels {
    pub(crate) warp: wgpu::RenderPipeline,
    pub(crate) half: wgpu::RenderPipeline,
    pub(crate) bgl: wgpu::BindGroupLayout,
}

fn pipeline(device: &wgpu::Device, module: &wgpu::ShaderModule, bgl: &wgpu::BindGroupLayout, fs: &str, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(fs), bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs),
        layout: Some(&layout),
        vertex: wgpu::VertexState { module, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl WarpKernels {
    /// The kernels writing `format` (the warp) and [`LEVEL_FORMAT`] (the halvings); None when the
    /// build fails (reported, not raised).
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Option<WarpKernels> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-comp-warp"),
            source: wgpu::ShaderSource::Wgsl(include_str!("comp_warp.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp-warp"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let warp = pipeline(device, &module, &bgl, "fs_warp", format);
        let half = pipeline(device, &module, &bgl, "fs_half", LEVEL_FORMAT);
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("comp warp kernels failed to build, corner pins render on the CPU: {e}");
            return None;
        }
        Some(WarpKernels { warp, half, bgl })
    }
}

/// How a warp of a `w`×`h` source through `hm` runs: the halved level sizes in order (empty: the
/// source itself is sampled) and the inverse map from destination pixels to the sampled level.
/// None when the CPU draws nothing (a singular or non-finite map at any level).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WarpLevels {
    pub(crate) sizes: Vec<(u32, u32)>,
    pub(crate) inv: Homography,
}

/// The CPU's recursion in [`warp_projective`](filmcraft_render::comp::track::warp_projective),
/// step for step.
pub(crate) fn levels(w: u32, h: u32, hm: &Homography) -> Option<WarpLevels> {
    const HALF: Homography = Homography { m: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0] };
    let (mut sw, mut sh) = (w as usize, h as usize);
    let mut hm = *hm;
    let mut sizes = Vec::new();
    loop {
        let inv = hm.inverse()?;
        let c = Vec2::new(sw as f64 / 2.0, sh as f64 / 2.0);
        let area = match (hm.apply(c), hm.apply(c + Vec2::new(1.0, 0.0)), hm.apply(c + Vec2::new(0.0, 1.0))) {
            (Some(p), Some(px), Some(py)) => {
                let (u, v) = (px - p, py - p);
                (u.x * v.y - u.y * v.x).abs()
            }
            _ => 1.0,
        };
        if area.is_finite() && area < 0.25 && sw >= 4 && sh >= 4 && sizes.len() < MAX_LEVELS {
            (sw, sh) = ((sw / 2).max(1), (sh / 2).max(1));
            sizes.push((u32::try_from(sw).ok()?, u32::try_from(sh).ok()?));
            hm = hm.after(&HALF);
            continue;
        }
        return Some(WarpLevels { sizes, inv });
    }
}

/// GPU memory the halved levels of a `w`×`h` source can take at most (every level down to 4
/// pixels).
pub(crate) fn levels_bytes(w: u32, h: u32) -> u64 {
    let (mut sw, mut sh) = (u64::from(w), u64::from(h));
    let mut bytes = 0u64;
    for _ in 0..MAX_LEVELS {
        if sw < 4 || sh < 4 {
            break;
        }
        (sw, sh) = ((sw / 2).max(1), (sh / 2).max(1));
        bytes = bytes.saturating_add(sw.saturating_mul(sh).saturating_mul(LEVEL_TEXEL_BYTES));
    }
    bytes
}

/// The uniform block of a warp (or halving) draw: the inverse map's rows and the sampled level's
/// size.
pub(crate) fn params(inv: &Homography, size: (u32, u32)) -> Vec<u8> {
    let m = inv.m.map(|v| v as f32);
    let vals = [m[0], m[1], m[2], 0.0, m[3], m[4], m[5], 0.0, m[6], m[7], m[8], 0.0, size.0 as f32, size.1 as f32, 0.0, 0.0];
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_cpu_recursion() {
        // identity: no halving
        let l = levels(100, 60, &Homography::IDENTITY).expect("invertible");
        assert!(l.sizes.is_empty());
        assert_eq!(l.inv, Homography::IDENTITY);
        // shrunk 8×: area 1/64 → halved twice (1/16, then 1/4 stops)
        let s = Homography { m: [0.125, 0.0, 0.0, 0.0, 0.125, 0.0, 0.0, 0.0, 1.0] };
        let l = levels(101, 61, &s).expect("invertible");
        assert_eq!(l.sizes, vec![(50, 30), (25, 15)]);
        // a source of 4 pixels or fewer is never halved
        assert!(levels(3, 300, &s).is_some_and(|l| l.sizes.is_empty()));
        // singular / non-finite maps draw nothing
        assert!(levels(10, 10, &Homography { m: [0.0; 9] }).is_none());
        assert!(levels(10, 10, &Homography { m: [f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] }).is_none());
        // a vanishing map stops at the smallest level instead of looping
        let tiny = Homography { m: [1e-5, 0.0, 0.0, 0.0, 1e-5, 0.0, 0.0, 0.0, 1.0] };
        let l = levels(6240, 4160, &tiny).expect("invertible");
        assert!(l.sizes.last().is_some_and(|&(w, h)| w.min(h) < 4 || l.sizes.len() == MAX_LEVELS));
        assert!(levels_bytes(6240, 4160) < 6240 * 4160 * 16 / 3 + 1);
        assert_eq!(levels_bytes(3, 3), 0);
    }
}

/// C3.8 review: the kernel maps destination pixels in f32 (`fs_warp`), the CPU in f64 (then
/// rounds the sample position to f32). At 6240 × 4160 with strong perspective (floor planes,
/// a vanishing far edge, a horizon outside the frame, minified levels) the f32 map stays within
/// a few thousandths of a source pixel of the CPU's, everywhere the CPU samples.
#[cfg(test)]
mod precision_tests {
    use super::*;
    use filmcraft_geom::Vec2;

    /// `fs_warp`'s map in f32, from the uniform block.
    fn shader_map(p: &[u8], x: f32, y: f32) -> Option<(f32, f32)> {
        let f: Vec<f32> = p.chunks(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let w = f[8] * x + f[9] * y + f[10];
        if w.abs().is_nan() || w.abs() < 1e-12 {
            return None;
        }
        Some(((f[0] * x + f[1] * y + f[2]) / w, (f[4] * x + f[5] * y + f[6]) / w))
    }

    #[test]
    fn the_f32_map_matches_the_cpu_at_6k_with_strong_perspective() {
        let (w, h) = (6240usize, 4160usize);
        let src = filmcraft_render::comp::track::frame_quad(w as f64, h as f64);
        for quad in [
            [(2900.0, 1500.0), (3340.0, 1500.0), (6240.0, 4160.0), (0.0, 4160.0)],
            [(3100.0, 2000.0), (3140.0, 2000.0), (6240.0, 4160.0), (0.0, 4160.0)],
            [(0.0, 0.0), (6240.0, 2000.0), (6240.0, 2160.0), (0.0, 4160.0)],
            [(-20000.0, -9000.0), (6000.0, 100.0), (6200.0, 4000.0), (-30000.0, 14000.0)],
            [(600.0, 400.0), (5800.0, 100.0), (6100.0, 4100.0), (300.0, 3900.0)],
        ] {
            let dst: Vec<Vec2> = quad.iter().map(|&(x, y)| Vec2::new(x, y)).collect();
            let hm = filmcraft_render::track::fit_homography(&src, &dst).expect("a fit");
            let plan = levels(w as u32, h as u32, &hm).expect("a regular map");
            let size = plan.sizes.last().copied().unwrap_or((w as u32, h as u32));
            let pr = params(&plan.inv, size);
            let (mut max, mut n) = (0f64, 0usize);
            for y in (0..h).step_by(3) {
                for x in (0..w).step_by(3) {
                    let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                    let Some(s) = plan.inv.apply(Vec2::new(fx, fy)) else { continue };
                    if s.x < -1.0 || s.y < -1.0 || s.x > size.0 as f64 + 1.0 || s.y > size.1 as f64 + 1.0 {
                        continue;
                    }
                    let (gx, gy) = shader_map(&pr, fx as f32, fy as f32).expect("the kernel samples where the CPU does");
                    max = max.max(f64::from((gx - s.x as f32).abs()).max(f64::from((gy - s.y as f32).abs())));
                    n += 1;
                }
            }
            assert!(n > 10_000, "{quad:?}: only {n} pixels drawn");
            assert!(max < 4e-3, "{quad:?}: f32 map off by {max} source pixels");
        }
    }
}
