// Comp Paint and Roto kernels (fork C3.6), the GPU side of `filmcraft_render::paint::apply_into`
// and the comp's `Pass::Coverage`. Both draw into an Rgba16Float slot texture.
//
// Paint: one full-screen draw. Each pixel starts from the node's input `src` (non-finite channels
// as 0, as `paint::apply` cleans every pixel), then walks its 16×16 tile's dab list (dab indices
// in stroke order, as the CPU lays them): the dabs of one stroke combine by maximum into the
// stroke matte `m`, and when the stroke changes, the finished stroke composites once (Color
// "over", Clone / Reveal / Erase `out += (T − out)·m`). Clone and Erase read `src` (the picture
// before any stroke), Reveal the second input, so pixels never depend on each other.

// `tiles_x`: tiles per row (`ceil(size.x / tile)`).
struct Params {
    size: vec2<u32>,
    tile: u32,
    tiles_x: u32,
};

// One dab in working pixels (filmcraft_render::paint::Dab) and the stroke it belongs to.
struct Dab {
    x: f32,
    y: f32,
    r: f32,
    hardness: f32,
    weight: f32,
    stroke: u32,
};

// tool: 0 Color, 1 Clone, 2 Reveal, 3 Erase; (ox, oy) the clone offset in whole pixels;
// source: the clone source (0 `src`, 1..4 `clone0`..`clone3`: the input at another time, C3.10).
struct Stroke {
    color: vec4<f32>,
    tool: u32,
    ox: i32,
    oy: i32,
    source: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var reveal: texture_2d<f32>;
@group(0) @binding(3) var<storage, read> dabs: array<Dab>;
@group(0) @binding(4) var<storage, read> strokes: array<Stroke>;
// per tile, row-major: (first index, index count)
@group(0) @binding(5) var<storage, read> tiles: array<vec2<u32>>;
@group(0) @binding(6) var<storage, read> index: array<u32>;
@group(0) @binding(10) var clone0: texture_2d<f32>;
@group(0) @binding(11) var clone1: texture_2d<f32>;
@group(0) @binding(12) var clone2: texture_2d<f32>;
@group(0) @binding(13) var clone3: texture_2d<f32>;

fn clone_load(source: u32, q: vec2<i32>) -> vec4<f32> {
    switch source {
        case 1u: { return textureLoad(clone0, q, 0); }
        case 2u: { return textureLoad(clone1, q, 0); }
        case 3u: { return textureLoad(clone2, q, 0); }
        case 4u: { return textureLoad(clone3, q, 0); }
        default: { return textureLoad(src, q, 0); }
    }
}

// Non-finite channels as 0 (the CPU's `is_finite` filter).
fn finite(v: vec4<f32>) -> vec4<f32> {
    let e = bitcast<vec4<u32>>(v) & vec4(0x7f800000u);
    return select(v, vec4(0.0), e == vec4(0x7f800000u));
}

// Coverage at distance `d` from a dab's centre: `Dab::profile` (keep the two in step).
fn profile(r: f32, hardness: f32, d: f32) -> f32 {
    let w = r * (1.0 - hardness);
    let c = r - w * 0.5;
    let u = clamp((c - d) / max(w, 1.0) + 0.5, 0.0, 1.0);
    let smooth_u = u * u * (3.0 - 2.0 * u);
    return u + (smooth_u - u) * min(w, 1.0);
}

fn composite(s: Stroke, m: f32, out: vec4<f32>, p: vec2<i32>) -> vec4<f32> {
    if s.tool == 0u {
        let a = m * s.color.a;
        return vec4(s.color.rgb * a + out.rgb * (1.0 - a), a + out.a * (1.0 - a));
    }
    var t: vec4<f32>;
    if s.tool == 1u {
        let q = p + vec2(s.ox, s.oy);
        let size = vec2<i32>(params.size);
        if q.x < 0 || q.y < 0 || q.x >= size.x || q.y >= size.y {
            return out;
        }
        t = finite(clone_load(s.source, q));
    } else if s.tool == 2u {
        t = finite(textureLoad(reveal, p, 0));
    } else {
        t = finite(textureLoad(src, p, 0));
    }
    return out + (t - out) * m;
}

@fragment
fn fs_paint(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let centre = vec2<f32>(p) + 0.5;
    var out = finite(textureLoad(src, p, 0));
    let tp = vec2<u32>(p) / params.tile;
    let t = tiles[tp.y * params.tiles_x + tp.x];
    let none = 0xffffffffu;
    var cur = none;
    var m = 0.0;
    for (var k = 0u; k < t.y; k = k + 1u) {
        let d = dabs[index[t.x + k]];
        if d.stroke != cur {
            if cur != none && m > 0.0 {
                out = composite(strokes[cur], m, out, p);
            }
            cur = d.stroke;
            m = 0.0;
        }
        let v = centre - vec2(d.x, d.y);
        m = max(m, profile(d.r, d.hardness, sqrt(v.x * v.x + v.y * v.y)) * d.weight);
    }
    if cur != none && m > 0.0 {
        out = composite(strokes[cur], m, out, p);
    }
    return out;
}

// Roto coverage: `mode` 1 = Matte (c, c, c, 1), 0 = Alpha (src · c).
struct CoverParams {
    mode: u32,
    _a: u32,
    _b: u32,
    _c: u32,
};

@group(0) @binding(7) var<uniform> cover: CoverParams;
@group(0) @binding(8) var cover_src: texture_2d<f32>;
@group(0) @binding(9) var coverage: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_cover(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let c = textureLoad(coverage, p, 0).r;
    if cover.mode == 1u {
        return vec4(c, c, c, 1.0);
    }
    return textureLoad(cover_src, p, 0) * c;
}
