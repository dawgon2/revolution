// nn: 2 × 2 max pooling (stride 2, floor) of a batch of h × w token grids read through a row
// stride and channel offset: Hiera's query pooling straight out of a fused qkv buffer
// (fork G3.4). Input token (b, y, x) channel ch is x[(b·h·w + y·w + x)·stride + off + ch];
// the output is contiguous [b][h/2][w/2][c].

struct Params {
    total: u32,
    c: u32,
    h: u32,
    w: u32,
    stride: u32,
    off: u32,
    _p0: u32,
    _p1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    let oh = p.h / 2u;
    let ow = p.w / 2u;
    let ch = idx % p.c;
    let tok = idx / p.c;
    let ox = tok % ow;
    let oy = (tok / ow) % oh;
    let b = tok / (ow * oh);
    var m = -3.0e38;
    for (var dy = 0u; dy < 2u; dy++) {
        for (var dx = 0u; dx < 2u; dx++) {
            let row = b * p.h * p.w + (oy * 2u + dy) * p.w + ox * 2u + dx;
            m = max(m, x[row * p.stride + p.off + ch]);
        }
    }
    o[idx] = m;
}
