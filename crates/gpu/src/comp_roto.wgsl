// Roto coverage rasteriser (fork LO.5): the GPU side of `filmcraft_render::roto::rasterize`.
// The CPU flattens the shapes (`roto::prepare`, the same edges as the CPU rasteriser), lists each
// pixel row's sorted edge crossings with their running winding, and bins the edges into 16×16
// tiles by their band. One thread per pixel then evaluates exactly the CPU's definition
// (`PreparedShape::coverage_at`): winding from the row's crossings left of the pixel centre,
// then the minimum (inside) / maximum (outside) of the per-edge feather ramps of the edges whose
// band reaches the pixel, then opacity / inversion and the shape modes top to bottom.

struct Params {
    size: vec2<u32>,
    n_shapes: u32,
    start: f32,
    // geom index of the first crossing x; index index of the first crossing's running winding
    cross_base: u32,
    cum_base: u32,
    tile: u32,
    _pad: u32,
};

// One shape (see `comp_roto::pack`). Rows `row0 .. row0 + nrows` have crossing lists:
// `index[row_base + r] .. index[row_base + r + 1]` (crossing numbers). Tiles in the rectangle
// `(tx0, ty0) + (tw, th)` have edge lists: `index[tile_base + i] .. index[tile_base + i + 1]`
// (positions in `index` holding edge numbers).
struct Shape {
    n_edges: u32,
    mode: u32,
    inverted: u32,
    opacity: f32,
    expansion: f32,
    row0: u32,
    nrows: u32,
    row_base: u32,
    tx0: u32,
    ty0: u32,
    tw: u32,
    th: u32,
    tile_base: u32,
    _a: u32,
    _b: u32,
    _c: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> shapes: array<Shape>;
@group(0) @binding(2) var<storage, read> index: array<u32>;
// edges (9 floats each: ax ay bx by a.inner a.outer b.inner b.outer band), then crossing x's
@group(0) @binding(3) var<storage, read> geom: array<f32>;
@group(0) @binding(4) var out: texture_storage_2d<r32float, write>;

// `filmcraft_render::mask::falloff` (keep the two in step).
fn falloff(s: f32, feather: f32) -> f32 {
    let w = max(feather, 1.0);
    let u = clamp(s / w + 0.5, 0.0, 1.0);
    let smooth_u = u * u * (3.0 - 2.0 * u);
    return u + (smooth_u - u) * min(feather, 1.0);
}

// `RotoMode::combine`: 0 Add, 1 Subtract, 2 Intersect, 3 Replace.
fn combine(mode: u32, a: f32, m: f32) -> f32 {
    switch mode {
        case 1u: { return a * (1.0 - m); }
        case 2u: { return a * m; }
        case 3u: { return m; }
        default: { return a + m - a * m; }
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.size.x || gid.y >= params.size.y {
        return;
    }
    let px = f32(gid.x) + 0.5;
    let py = f32(gid.y) + 0.5;
    let tp = gid.xy / params.tile;
    var acc = params.start;
    for (var si = 0u; si < params.n_shapes; si = si + 1u) {
        let s = shapes[si];
        var c = 0.0;
        if s.n_edges > 0u {
            // winding: the running winding after the last crossing left of the pixel centre
            var wind = 0;
            if gid.y >= s.row0 && gid.y - s.row0 < s.nrows {
                let r = gid.y - s.row0;
                let lo = index[s.row_base + r];
                var a = lo;
                var b = index[s.row_base + r + 1u];
                while a < b {
                    let m = a + (b - a) / 2u;
                    if geom[params.cross_base + m] < px {
                        a = m + 1u;
                    } else {
                        b = m;
                    }
                }
                if a > lo {
                    wind = bitcast<i32>(index[params.cum_base + a - 1u]);
                }
            }
            let inside = wind != 0;
            c = select(0.0, 1.0, inside);
            if tp.x >= s.tx0 && tp.x - s.tx0 < s.tw && tp.y >= s.ty0 && tp.y - s.ty0 < s.th {
                let ti = s.tile_base + (tp.y - s.ty0) * s.tw + (tp.x - s.tx0);
                let end = index[ti + 1u];
                for (var k = index[ti]; k < end; k = k + 1u) {
                    let g = index[k] * 9u;
                    let ax = geom[g];
                    let ay = geom[g + 1u];
                    let dx = geom[g + 2u] - ax;
                    let dy = geom[g + 3u] - ay;
                    let qx = px - ax;
                    let qy = py - ay;
                    let l2 = dx * dx + dy * dy;
                    var t = 0.0;
                    if l2 > 0.0 {
                        t = clamp((qx * dx + qy * dy) / l2, 0.0, 1.0);
                    }
                    let ex = qx - dx * t;
                    let ey = qy - dy * t;
                    let d2 = ex * ex + ey * ey;
                    let band = geom[g + 8u];
                    if d2 > band * band {
                        continue;
                    }
                    let d = sqrt(d2);
                    let sd = select(-d, d, inside) + s.expansion;
                    let wi = geom[g + 4u] + (geom[g + 6u] - geom[g + 4u]) * t;
                    let wo = geom[g + 5u] + (geom[g + 7u] - geom[g + 5u]) * t;
                    let v = falloff(sd - (wi - wo) * 0.5, wi + wo);
                    c = select(max(c, v), min(c, v), inside);
                }
            }
        }
        let value = s.opacity * select(c, 1.0 - c, s.inverted != 0u);
        acc = clamp(combine(s.mode, acc, value), 0.0, 1.0);
    }
    // (non-finite: 0, as the CPU program cleans its raster)
    let e = bitcast<u32>(acc) & 0x7f800000u;
    if e == 0x7f800000u {
        acc = 0.0;
    }
    textureStore(out, vec2<i32>(gid.xy), vec4(acc, 0.0, 0.0, 1.0));
}
