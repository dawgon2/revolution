//! Paint and Roto passes of comp programs on the GPU (fork C3.6, `comp_paint.wgsl`).
//!
//! **Paint.** The CPU plans the strokes exactly as [`filmcraft_render::paint::apply`] does
//! ([`filmcraft_render::paint::plan`]: the same dabs, budgets and cleaned values), bins the dabs
//! into 16 × 16 pixel tiles (each tile's list in stroke order) and uploads four small buffers; one
//! full-screen draw writes every pixel into the scratch slot (non-finite input channels as 0, as
//! the CPU's `apply` does for the whole frame) and the slots swap. Clone / Erase read the input as it was before the pass (Clone
//! strokes with a clone time offset one of up to four bound pictures of the input at another
//! time, C3.10), Reveal the second input, so one draw does all strokes. Work is bounded: at most [`MAX_TILE_ENTRIES`] (tile, dab)
//! pairs and buffers within the device's binding limits; anything larger, a device without the
//! kernels or a GPU error falls back to the read-back CPU path.
//!
//! **Roto.** The coverage is an `R32Float` texture (no 8-bit matte) filled by the roto kernel
//! (LO.5, [`crate::comp_roto`]) or, when that can't run, uploaded from the CPU rasteriser (see
//! [`filmcraft_render::roto`]); one full-screen draw writes the Matte output `(c, c, c, 1)` or
//! the Alpha output `src · c`.

use filmcraft_project::paint::PaintTool;
use filmcraft_render::paint::PlannedStroke;

/// Tile side (pixels) of the paint kernel's dab bins.
pub(crate) const TILE: usize = 16;
/// Most (tile, dab) pairs one Paint pass uploads (each costs a tile of pixel evaluations).
pub(crate) const MAX_TILE_ENTRIES: usize = 8 << 20;
/// Clone-from-another-frame sources the kernel binds (C3.10; stroke source `i` reads `clone{i-1}`).
pub(crate) const CLONE_SOURCES: usize = filmcraft_render::paint::MAX_CLONE_TIMES;
const _: () = assert!(CLONE_SOURCES == 4, "comp_paint.wgsl binds four clone sources");
/// Largest clone offset passed to the kernel (larger ones fall outside any frame anyway).
const MAX_OFFSET: i64 = 1 << 30;
const DAB_BYTES: usize = 24;
const STROKE_BYTES: usize = 32;
const TILE_BYTES: usize = 8;

/// The two pipelines and their layouts.
pub(crate) struct CompKernels {
    pub(crate) paint: wgpu::RenderPipeline,
    pub(crate) paint_bgl: wgpu::BindGroupLayout,
    pub(crate) cover: wgpu::RenderPipeline,
    pub(crate) cover_bgl: wgpu::BindGroupLayout,
}

fn tex(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn storage(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    bgl: &wgpu::BindGroupLayout,
    vs: &str,
    fs: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(fs), bind_group_layouts: &[Some(bgl)], immediate_size: 0 });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs),
        layout: Some(&layout),
        vertex: wgpu::VertexState { module, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
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

impl CompKernels {
    /// The kernels for `format` targets; None when the device lacks what they need (four storage
    /// buffers in the fragment stage) or the build fails (reported, not raised).
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Option<CompKernels> {
        if device.limits().max_storage_buffers_per_shader_stage < 4 {
            return None;
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-comp-paint"),
            source: wgpu::ShaderSource::Wgsl(include_str!("comp_paint.wgsl").into()),
        });
        let frag = wgpu::ShaderStages::FRAGMENT;
        let paint_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp-paint"),
            entries: &[uniform(0), tex(1), tex(2), storage(3, frag), storage(4, frag), storage(5, frag), storage(6, frag), tex(10), tex(11), tex(12), tex(13)],
        });
        let cover_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("comp-cover"), entries: &[uniform(7), tex(8), tex(9)] });
        let paint = pipeline(device, &module, &paint_bgl, "vs_full", "fs_paint", format);
        let cover = pipeline(device, &module, &cover_bgl, "vs_full", "fs_cover", format);
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("comp paint kernels failed to build, painting on the CPU: {e}");
            return None;
        }
        Some(CompKernels { paint, paint_bgl, cover, cover_bgl })
    }
}

/// One Paint pass's buffer contents.
#[derive(Debug)]
pub(crate) struct PaintData {
    pub(crate) dabs: Vec<u8>,
    pub(crate) strokes: Vec<u8>,
    /// Every tile's (first index, count), row-major, `tiles_x` per row.
    pub(crate) tiles: Vec<u32>,
    pub(crate) index: Vec<u32>,
    pub(crate) tiles_x: u32,
}

fn tool_code(t: PaintTool) -> u32 {
    match t {
        PaintTool::Color => 0,
        PaintTool::Clone => 1,
        PaintTool::Reveal => 2,
        PaintTool::Erase => 3,
    }
}

/// Packs planned strokes on a `w × h` frame into the kernel's buffers, the dabs binned per tile.
/// None when there is nothing to draw, or the pass has more than `max_entries` (tile, dab) pairs
/// ([`MAX_TILE_ENTRIES`]) or a buffer over `max_bytes` (the caller paints on the CPU then).
pub(crate) fn pack(strokes: &[PlannedStroke], w: usize, h: usize, max_bytes: u64, max_entries: usize) -> Option<PaintData> {
    let (tx, ty) = (w.div_ceil(TILE), h.div_ceil(TILE));
    let n_tiles = tx.checked_mul(ty)?;
    let n_dabs: usize = strokes.iter().map(|s| s.dabs.len()).fold(0, usize::saturating_add);
    if n_tiles == 0 || n_dabs == 0 || u32::try_from(n_dabs).is_err() || u32::try_from(strokes.len()).is_err() {
        return None;
    }
    let tile_range = |b: &[usize; 4]| (b[0] / TILE, b[2].div_ceil(TILE).min(tx), b[1] / TILE, b[3].div_ceil(TILE).min(ty));
    // count per tile, capped
    let mut counts = vec![0u32; n_tiles];
    let mut total = 0usize;
    for (_, b) in strokes.iter().flat_map(|s| s.dabs.iter()) {
        let (x0, x1, y0, y1) = tile_range(b);
        total = total.saturating_add(x1.saturating_sub(x0).saturating_mul(y1.saturating_sub(y0)));
        if total > max_entries.min(MAX_TILE_ENTRIES) {
            return None;
        }
        for y in y0..y1 {
            for x in x0..x1 {
                let c = counts.get_mut(y * tx + x)?;
                *c = c.saturating_add(1);
            }
        }
    }
    let fits = |n: usize, bytes: usize| n.checked_mul(bytes).is_some_and(|b| (b as u64) <= max_bytes);
    if total == 0 || !fits(n_dabs, DAB_BYTES) || !fits(strokes.len(), STROKE_BYTES) || !fits(n_tiles, TILE_BYTES) || !fits(total, 4) {
        return None;
    }
    let tiles_x = u32::try_from(tx).ok()?;
    // first index of every tile; every tile's record
    let mut cursor = vec![0u32; n_tiles];
    let mut tiles = Vec::with_capacity(n_tiles.saturating_mul(2));
    let mut start = 0u32;
    for (c, cur) in counts.iter().zip(cursor.iter_mut()) {
        *cur = start;
        tiles.extend_from_slice(&[start, *c]);
        start = start.saturating_add(*c);
    }
    let mut index = vec![0u32; total];
    let mut dabs = Vec::with_capacity(n_dabs * DAB_BYTES);
    let mut packed = Vec::with_capacity(strokes.len() * STROKE_BYTES);
    let mut dab_i = 0u32;
    for (si, s) in strokes.iter().enumerate() {
        let off = |v: i64| v.clamp(-MAX_OFFSET, MAX_OFFSET) as i32;
        for v in s.color {
            packed.extend_from_slice(&v.to_le_bytes());
        }
        packed.extend_from_slice(&tool_code(s.tool).to_le_bytes());
        packed.extend_from_slice(&off(s.offset.0).to_le_bytes());
        packed.extend_from_slice(&off(s.offset.1).to_le_bytes());
        // (a source the kernel doesn't bind: the input at the same time, as the CPU does)
        let source = if (s.source as usize) <= CLONE_SOURCES { s.source } else { 0 };
        packed.extend_from_slice(&source.to_le_bytes());
        for (d, b) in &s.dabs {
            for v in [d.x, d.y, d.r, d.hardness, d.weight] {
                dabs.extend_from_slice(&v.to_le_bytes());
            }
            dabs.extend_from_slice(&(si as u32).to_le_bytes());
            let (x0, x1, y0, y1) = tile_range(b);
            for y in y0..y1 {
                for x in x0..x1 {
                    let cur = cursor.get_mut(y * tx + x)?;
                    *index.get_mut(*cur as usize)? = dab_i;
                    *cur = cur.saturating_add(1);
                }
            }
            dab_i = dab_i.saturating_add(1);
        }
    }
    Some(PaintData { dabs, strokes: packed, tiles, index, tiles_x })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_render::paint::Dab;

    fn stroke(tool: PaintTool, dabs: &[(f32, f32, f32)], w: usize, h: usize) -> PlannedStroke {
        let list: Vec<(Dab, [usize; 4])> = dabs
            .iter()
            .map(|&(x, y, r)| {
                let c = |v: f32, hi: usize| v.clamp(0.0, hi as f32) as usize;
                let b = [c((x - r - 1.0).floor(), w), c((y - r - 1.0).floor(), h), c((x + r + 1.0).ceil(), w), c((y + r + 1.0).ceil(), h)];
                (Dab { x, y, r, hardness: 0.5, weight: 1.0 }, b)
            })
            .collect();
        let area = list.iter().fold([usize::MAX, usize::MAX, 0, 0], |a, (_, b)| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
        PlannedStroke { tool, color: [1.0; 4], offset: (i64::MAX, i64::MIN), source: 7, dabs: list, area }
    }

    fn u32s(b: &[u8]) -> Vec<u32> {
        b.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect()
    }

    #[test]
    fn dabs_are_binned_per_tile_in_stroke_order() {
        let (w, h) = (40, 20);
        let a = stroke(PaintTool::Color, &[(5.0, 5.0, 2.0), (30.0, 10.0, 3.0)], w, h);
        let b = stroke(PaintTool::Clone, &[(6.0, 6.0, 2.0)], w, h);
        let d = pack(&[a, b], w, h, u64::MAX, MAX_TILE_ENTRIES).expect("packs");
        let index = &d.index;
        // 3 × 2 tiles, row-major; tile (0, 0) holds dab 0 then dab 2 (stroke order); dab 1
        // (box 26..34 × 6..14) spans tiles (1, 0) and (2, 0)
        assert_eq!(d.tiles_x, 3);
        let rec: Vec<&[u32]> = d.tiles.chunks(2).collect();
        assert_eq!(rec.len(), 6, "every tile has a record");
        assert_eq!(&index[rec[0][0] as usize..(rec[0][0] + rec[0][1]) as usize], &[0, 2]);
        for tx in [1, 2] {
            assert!(rec[tx][1] == 1 && index[rec[tx][0] as usize] == 1, "tile ({tx}, 0)");
        }
        assert!(rec[3..].iter().all(|t| t[1] == 0), "no other tile");
        assert_eq!(index.len(), 4);
        // offsets are clamped to the kernel's range
        let s = u32s(&d.strokes);
        assert_eq!(s[5] as i32, 1 << 30);
        assert_eq!(s[6] as i32, -(1 << 30));
        // a clone source the kernel doesn't bind falls back to the input
        assert_eq!(s[7], 0);
    }

    #[test]
    fn hostile_sizes_are_refused_not_panicking() {
        let (w, h) = (16_000, 16_000);
        // one huge dab covers a million tiles
        let big = stroke(PaintTool::Color, &[(8000.0, 8000.0, 9000.0)], w, h);
        assert!(pack(std::slice::from_ref(&big), w, h, u64::MAX, MAX_TILE_ENTRIES).is_some());
        let many = PlannedStroke { dabs: big.dabs.repeat(9), ..big.clone() };
        assert!(pack(&[many], w, h, u64::MAX, MAX_TILE_ENTRIES).is_none(), "over the entry cap");
        let small = stroke(PaintTool::Erase, &[(3.0, 3.0, 1.0)], 8, 8);
        assert!(pack(std::slice::from_ref(&small), 8, 8, 8, MAX_TILE_ENTRIES).is_none(), "over the binding limit");
        assert!(pack(&[], 8, 8, u64::MAX, MAX_TILE_ENTRIES).is_none());
        assert!(pack(std::slice::from_ref(&small), 0, 8, u64::MAX, MAX_TILE_ENTRIES).is_none());
        assert!(pack(std::slice::from_ref(&small), 8, 8, u64::MAX, 0).is_none(), "a lowered cap");
        // boxes beyond the frame (hostile planner input) are clipped to the tiles
        let mut odd = small;
        odd.dabs[0].1 = [100, 100, usize::MAX, usize::MAX];
        assert!(pack(&[odd], 8, 8, u64::MAX, MAX_TILE_ENTRIES).is_none(), "nothing on the frame");
    }
}
