//! Roto coverage on the GPU (fork LO.5, `comp_roto.wgsl`).
//!
//! The CPU still evaluates and flattens the shapes ([`filmcraft_render::roto::prepare`]: keyframes,
//! linked tracker motion, Bézier / B-spline flattening, feather and expansion in working pixels),
//! which is cheap; the per-pixel work moves to a compute kernel. [`pack`] lays out, per enabled
//! shape:
//!
//! - its edges exactly as the CPU rasteriser measures them ([`PreparedShape::edges`]: end points,
//!   inner / outer feather at both ends, band);
//! - for every pixel row the shape spans, the x of each edge crossing at the row's pixel centre
//!   (computed here with the CPU's own [`Edge::crosses`], so the winding is bit-identical),
//!   sorted, with the running winding after each, so a pixel's winding is one binary search;
//! - for every 16 × 16 tile of the shape's bounding rectangle, the edges whose band can reach a
//!   pixel of the tile.
//!
//! The kernel then computes, per pixel centre, the CPU's definition
//! ([`PreparedShape::coverage_at`]): the min (inside) / max (outside) of the edges' feather ramps,
//! opacity, inversion and the shape modes, into the comp's `R32Float` coverage texture. The only
//! differences from the CPU are float rounding in the distance (GPU division / square root and
//! fused multiply-adds), see the parity test in `comp_tests.rs`.
//!
//! Work is bounded: at most [`MAX_CROSSINGS`] row crossings and [`MAX_TILE_ENTRIES`] (tile, edge)
//! pairs, every buffer within the device's binding limits, all offsets in `u32`; anything larger
//! (or a device without compute / storage textures, or a GPU error) falls back to the CPU raster,
//! uploaded as before (C3.6).

use filmcraft_project::roto::RotoMode;
use filmcraft_render::roto::{Edge, PreparedRoto, PreparedShape};

/// Tile side (pixels) of the edge bins (also the kernel's workgroup side).
pub(crate) const TILE: usize = 16;
/// Most (tile, edge) pairs one pass uploads.
pub(crate) const MAX_TILE_ENTRIES: usize = 8 << 20;
/// Most row crossings one pass uploads.
pub(crate) const MAX_CROSSINGS: usize = 8 << 20;
/// Floats per edge in `geom`.
const EDGE_FLOATS: usize = 9;
/// Words per shape record.
pub(crate) const SHAPE_WORDS: usize = 16;

/// The kernel and its layout.
pub(crate) struct RotoKernel {
    pub(crate) pipeline: wgpu::ComputePipeline,
    pub(crate) bgl: wgpu::BindGroupLayout,
}

fn buffer(binding: u32, uniform: bool) -> wgpu::BindGroupLayoutEntry {
    let ty = if uniform { wgpu::BufferBindingType::Uniform } else { wgpu::BufferBindingType::Storage { read_only: true } };
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

impl RotoKernel {
    /// None when the device lacks what the kernel needs (compute with three storage buffers, one
    /// storage texture, 16 × 16 workgroups) or the build fails (reported, not raised).
    pub(crate) fn new(device: &wgpu::Device) -> Option<RotoKernel> {
        let l = device.limits();
        let side = TILE as u32;
        if l.max_storage_buffers_per_shader_stage < 3
            || l.max_storage_textures_per_shader_stage < 1
            || l.max_compute_invocations_per_workgroup < side * side
            || l.max_compute_workgroup_size_x < side
            || l.max_compute_workgroup_size_y < side
        {
            return None;
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-comp-roto"),
            source: wgpu::ShaderSource::Wgsl(include_str!("comp_roto.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("comp-roto"),
            entries: &[
                buffer(0, true),
                buffer(1, false),
                buffer(2, false),
                buffer(3, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("comp-roto"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("comp-roto"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("the roto kernel failed to build, roto coverage is rasterised on the CPU: {e}");
            return None;
        }
        Some(RotoKernel { pipeline, bgl })
    }
}

/// One pass's buffer contents.
#[derive(Debug)]
pub(crate) struct RotoData {
    /// [`SHAPE_WORDS`] words per shape (see `comp_roto.wgsl`'s `Shape`).
    pub(crate) shapes: Vec<u32>,
    /// Row offsets, tile offsets, tile edge lists, then the crossings' running windings.
    pub(crate) index: Vec<u32>,
    /// Edges ([`EDGE_FLOATS`] each), then the crossings' x.
    pub(crate) geom: Vec<f32>,
    /// The uniform block (32 bytes).
    pub(crate) params: Vec<u8>,
}

fn mode_code(m: RotoMode) -> u32 {
    match m {
        RotoMode::Add => 0,
        RotoMode::Subtract => 1,
        RotoMode::Intersect => 2,
        RotoMode::Replace => 3,
    }
}

/// Rows whose pixel centre an edge may cross: a superset of [`Edge::crosses`]'s, clamped to
/// `0 .. h` (empty for an edge off the frame).
fn edge_rows(e: &Edge, h: usize) -> (usize, usize) {
    let (lo, hi) = (e.a.p[1].min(e.b.p[1]), e.a.p[1].max(e.b.p[1]));
    let clamp = |v: f32| f64::from(v).clamp(0.0, h as f64) as usize;
    (clamp((lo - 1.0).floor()), clamp((hi + 1.0).ceil()))
}

/// The tiles an edge's band reaches: `(x0, x1, y0, y1)` exclusive ends, empty off the frame.
fn edge_tiles(e: &Edge, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let r = e.reach;
    // pixel ranges a little wider than the band (the kernel tests the band exactly)
    let px = |v: f32, n: usize| f64::from(v).clamp(0.0, n as f64) as usize;
    let (x0, x1) = (px((r[0] - 1.0).floor(), w), px((r[2] + 2.0).ceil(), w));
    let (y0, y1) = (px((r[1] - 1.0).floor(), h), px((r[3] + 2.0).ceil(), h));
    if x0 >= x1 || y0 >= y1 {
        return (0, 0, 0, 0);
    }
    (x0 / TILE, x1.div_ceil(TILE), y0 / TILE, y1.div_ceil(TILE))
}

/// One shape's row crossings: `(row0, offsets (nrows + 1, relative), x, running winding)`.
/// None past `budget` crossings.
#[allow(clippy::type_complexity)]
fn crossings(edges: &[Edge], h: usize, budget: usize) -> Option<(usize, Vec<u32>, Vec<f32>, Vec<i32>)> {
    let (mut r0, mut r1, mut bound) = (h, 0usize, 0usize);
    for e in edges {
        let (a, b) = edge_rows(e, h);
        if a < b {
            r0 = r0.min(a);
            r1 = r1.max(b);
            bound = bound.saturating_add(b - a);
        }
    }
    if r0 >= r1 {
        return Some((0, vec![0], Vec::new(), Vec::new()));
    }
    // the candidate rows bound the work and the memory before anything is allocated
    if bound > budget {
        return None;
    }
    let n_rows = r1 - r0;
    let mut counts = vec![0u32; n_rows];
    let mut total = 0usize;
    for e in edges {
        let (a, b) = edge_rows(e, h);
        for y in a..b {
            if e.crosses(y as f32 + 0.5).is_some() {
                let c = counts.get_mut(y - r0)?;
                *c = c.saturating_add(1);
                total += 1;
            }
        }
    }
    let mut offsets = Vec::with_capacity(n_rows + 1);
    let mut start = 0u32;
    offsets.push(0);
    for c in &counts {
        start = start.checked_add(*c)?;
        offsets.push(start);
    }
    let mut cursor: Vec<u32> = offsets.get(..n_rows)?.to_vec();
    let mut row_x: Vec<(f32, i32)> = vec![(0.0, 0); total];
    for e in edges {
        let (a, b) = edge_rows(e, h);
        for y in a..b {
            if let Some(c) = e.crosses(y as f32 + 0.5) {
                let cur = cursor.get_mut(y - r0)?;
                *row_x.get_mut(*cur as usize)? = c;
                *cur += 1;
            }
        }
    }
    let mut xs = Vec::with_capacity(total);
    let mut cum = Vec::with_capacity(total);
    for r in offsets.windows(2) {
        let row = row_x.get_mut(r[0] as usize..r[1] as usize)?;
        row.sort_by(|p, q| p.0.total_cmp(&q.0));
        let mut wind = 0i32;
        for (x, d) in row.iter() {
            wind = wind.saturating_add(*d);
            xs.push(*x);
            cum.push(wind);
        }
    }
    Some((r0, offsets, xs, cum))
}

/// One shape's tile bins over its bounding tile rectangle: `((tx0, ty0, tw, th), offsets
/// (tw·th + 1, relative), edge numbers relative to the shape's first)`. None past `budget`
/// entries.
#[allow(clippy::type_complexity)]
fn tiles(edges: &[Edge], w: usize, h: usize, budget: usize) -> Option<((usize, usize, usize, usize), Vec<u32>, Vec<u32>)> {
    let ranges: Vec<_> = edges.iter().map(|e| edge_tiles(e, w, h)).collect();
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
    let mut total = 0usize;
    for &(a, b, c, d) in &ranges {
        if a < b && c < d {
            (x0, x1, y0, y1) = (x0.min(a), x1.max(b), y0.min(c), y1.max(d));
            total = total.saturating_add((b - a).saturating_mul(d - c));
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return Some(((0, 0, 0, 0), vec![0], Vec::new()));
    }
    if total > budget {
        return None;
    }
    let (tw, th) = (x1 - x0, y1 - y0);
    let n = tw.checked_mul(th)?;
    let mut counts = vec![0u32; n];
    for &(a, b, c, d) in &ranges {
        for ty in c..d {
            for tx in a..b {
                let k = counts.get_mut((ty - y0) * tw + (tx - x0))?;
                *k = k.saturating_add(1);
            }
        }
    }
    let mut offsets = Vec::with_capacity(n + 1);
    let mut start = 0u32;
    offsets.push(0);
    for c in &counts {
        start = start.checked_add(*c)?;
        offsets.push(start);
    }
    let mut cursor: Vec<u32> = offsets.get(..n)?.to_vec();
    let mut list = vec![0u32; total];
    for (i, &(a, b, c, d)) in ranges.iter().enumerate() {
        let id = u32::try_from(i).ok()?;
        for ty in c..d {
            for tx in a..b {
                let cur = cursor.get_mut((ty - y0) * tw + (tx - x0))?;
                *list.get_mut(*cur as usize)? = id;
                *cur += 1;
            }
        }
    }
    Some(((x0, y0, tw, th), offsets, list))
}

fn u32_of(v: usize) -> Option<u32> {
    u32::try_from(v).ok()
}

/// Packs the prepared shapes of a `w × h` frame into the kernel's buffers. None when the pass
/// needs more than `max_entries` (tile, edge) pairs ([`MAX_TILE_ENTRIES`]) or [`MAX_CROSSINGS`]
/// crossings, a buffer over `max_bytes`, or the frame is empty (the caller rasterises on the
/// CPU then).
pub(crate) fn pack(roto: &PreparedRoto, w: usize, h: usize, max_bytes: u64, max_entries: usize) -> Option<RotoData> {
    if w == 0 || h == 0 {
        return None;
    }
    let mut entry_budget = max_entries.min(MAX_TILE_ENTRIES);
    let mut cross_budget = MAX_CROSSINGS;
    let mut shapes: Vec<u32> = Vec::with_capacity(roto.shapes.len().saturating_mul(SHAPE_WORDS));
    let mut index: Vec<u32> = Vec::new();
    let mut geom: Vec<f32> = Vec::new();
    let mut xs_all: Vec<f32> = Vec::new();
    let mut cum_all: Vec<u32> = Vec::new();
    let fits = |n: usize| n.checked_mul(4).is_some_and(|b| (b as u64) <= max_bytes);
    for s in &roto.shapes {
        let PreparedShape { expansion, opacity, inverted, mode, .. } = *s;
        let edges = s.edges();
        let mut rec = [0u32; SHAPE_WORDS];
        rec[1] = mode_code(mode);
        rec[2] = u32::from(inverted);
        rec[3] = opacity.to_bits();
        rec[4] = expansion.to_bits();
        if !edges.is_empty() {
            rec[0] = u32_of(edges.len())?;
            let first_edge = u32_of(geom.len() / EDGE_FLOATS)?;
            for e in edges {
                geom.extend_from_slice(&[e.a.p[0], e.a.p[1], e.b.p[0], e.b.p[1], e.a.inner, e.a.outer, e.b.inner, e.b.outer, e.band]);
            }
            // rows
            let (row0, offsets, xs, cum) = crossings(edges, h, cross_budget)?;
            cross_budget = cross_budget.saturating_sub(xs.len());
            let cross0 = u32_of(xs_all.len())?;
            rec[5] = u32_of(row0)?;
            rec[6] = u32_of(offsets.len().saturating_sub(1))?;
            rec[7] = u32_of(index.len())?;
            for o in &offsets {
                index.push(cross0.checked_add(*o)?);
            }
            xs_all.extend_from_slice(&xs);
            cum_all.extend(cum.iter().map(|c| *c as u32));
            // tiles
            let ((tx0, ty0, tw, th), offsets, list) = tiles(edges, w, h, entry_budget)?;
            entry_budget = entry_budget.saturating_sub(list.len());
            rec[8] = u32_of(tx0)?;
            rec[9] = u32_of(ty0)?;
            rec[10] = u32_of(tw)?;
            rec[11] = u32_of(th)?;
            rec[12] = u32_of(index.len())?;
            // the lists follow the offsets
            let list0 = u32_of(index.len().checked_add(offsets.len())?)?;
            for o in &offsets {
                index.push(list0.checked_add(*o)?);
            }
            for id in &list {
                index.push(first_edge.checked_add(*id)?);
            }
            if !fits(index.len()) {
                return None;
            }
        }
        shapes.extend_from_slice(&rec);
    }
    let cross_base = u32_of(geom.len())?;
    geom.extend_from_slice(&xs_all);
    let cum_base = u32_of(index.len())?;
    index.extend_from_slice(&cum_all);
    if !fits(index.len()) || !fits(geom.len()) || !fits(shapes.len()) {
        return None;
    }
    let start = roto.shapes.first().map_or(0.0, |s| s.mode.start());
    let mut params = Vec::with_capacity(32);
    for v in [u32_of(w)?, u32_of(h)?, u32_of(roto.shapes.len())?, start.to_bits(), cross_base, cum_base, TILE as u32, 0] {
        params.extend_from_slice(&v.to_le_bytes());
    }
    Some(RotoData { shapes, index, geom, params })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_geom::Vec2;
    use filmcraft_project::MaskPath;
    use filmcraft_project::roto::{Roto, RotoKind, RotoShape};
    use filmcraft_time::Tick;

    fn prepared(shapes: Vec<RotoShape>, w: usize, h: usize) -> PreparedRoto {
        filmcraft_render::roto::prepare(&Roto { shapes }, Tick(0), w, h, 1.0)
    }

    #[test]
    fn rows_and_tiles_are_laid_out() {
        let sq = RotoShape::new("s", RotoKind::Bezier, MaskPath::rect(10.0, 10.0, 30.0, 20.0));
        let (w, h) = (64, 40);
        let d = pack(&prepared(vec![sq], w, h), w, h, u64::MAX, MAX_TILE_ENTRIES).expect("packs");
        assert_eq!(d.shapes.len(), SHAPE_WORDS);
        let s = &d.shapes;
        assert_eq!(s[0], 4, "four edges");
        // rows 10..20 have two crossings each (left and right edges), running winding back to 0
        let (row0, nrows, row_base) = (s[5] as usize, s[6] as usize, s[7] as usize);
        let p = |k: usize| u32::from_le_bytes(d.params[k * 4..k * 4 + 4].try_into().expect("4 bytes"));
        let (cross_base, cum_base) = (p(4) as usize, p(5) as usize);
        for y in 0..h {
            let n = if y >= row0 && y < row0 + nrows { d.index[row_base + y - row0 + 1] - d.index[row_base + y - row0] } else { 0 };
            assert_eq!(n, if (10..20).contains(&y) { 2 } else { 0 }, "row {y}");
            if n == 2 {
                let first = d.index[row_base + y - row0] as usize;
                assert_eq!((d.geom[cross_base + first], d.geom[cross_base + first + 1]), (10.0, 30.0));
                assert_eq!(d.index[cum_base + first + 1], 0, "back to winding 0");
                assert_ne!(d.index[cum_base + first], 0);
            }
        }
        // a 1.5 px band (and the binning margin): tiles 0..3 × 0..2
        assert_eq!((s[8], s[9], s[10], s[11]), (0, 0, 3, 2));
    }

    #[test]
    fn hostile_sizes_are_refused_not_panicking() {
        // a huge feather reaches every tile of a big frame: over the entry cap
        let mut big = RotoShape::new("s", RotoKind::Bezier, MaskPath::ellipse(Vec2::new(3000.0, 2000.0), Vec2::new(2000.0, 1500.0)));
        big.feather = filmcraft_project::Param::new(filmcraft_project::ParamValue::Float(1e5));
        let (w, h) = (6240, 4160);
        let p = prepared(vec![big], w, h);
        assert!(pack(&p, w, h, u64::MAX, MAX_TILE_ENTRIES).is_none());
        // a lowered cap, a binding limit, an empty frame
        let sq = RotoShape::new("s", RotoKind::Bezier, MaskPath::rect(1.0, 1.0, 6.0, 6.0));
        let p = prepared(vec![sq], 8, 8);
        assert!(pack(&p, 8, 8, u64::MAX, 0).is_none());
        assert!(pack(&p, 8, 8, 8, MAX_TILE_ENTRIES).is_none());
        assert!(pack(&p, 0, 8, u64::MAX, MAX_TILE_ENTRIES).is_none());
        // a shape entirely off the frame, and no shapes at all, still pack
        let off = RotoShape::new("o", RotoKind::Bezier, MaskPath::rect(-50.0, -50.0, -20.0, -20.0));
        let d = pack(&prepared(vec![off], 8, 8), 8, 8, u64::MAX, MAX_TILE_ENTRIES).expect("packs");
        assert_eq!((d.shapes[6], d.shapes[10]), (0, 0), "no rows, no tiles");
        assert!(pack(&PreparedRoto::default(), 8, 8, u64::MAX, MAX_TILE_ENTRIES).is_some());
    }
}
