// nn: depthwise k × k convolution (any stride / padding) on HWC images, weights [c][k·k] (a
// folded batch norm goes into w / bias); flag 1 = GELU (tanh form) after.

struct Params {
    total: u32,
    c: u32,
    in_h: u32,
    in_w: u32,
    out_w: u32,
    stride: u32,
    flags: u32,
    k: u32,
    pad: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read> w: array<f32>;
@group(0) @binding(3) var<storage, read> bias: array<f32>;
@group(0) @binding(4) var<storage, read_write> o: array<f32>;

fn gelu(v: f32) -> f32 {
    let inner = 0.7978845608028654 * v * (1.0 + 0.044715 * v * v);
    return 0.5 * v * (1.0 + tanh(clamp(inner, -15.0, 15.0)));
}

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    let ch = idx % p.c;
    let pix = idx / p.c;
    let oy = pix / p.out_w;
    let ox = pix % p.out_w;
    let kk = p.k * p.k;
    var sum = 0.0;
    for (var ky = 0u; ky < p.k; ky++) {
        let iy = i32(oy * p.stride + ky) - i32(p.pad);
        if iy < 0 || u32(iy) >= p.in_h {
            continue;
        }
        for (var kx = 0u; kx < p.k; kx++) {
            let ix = i32(ox * p.stride + kx) - i32(p.pad);
            if ix < 0 || u32(ix) >= p.in_w {
                continue;
            }
            sum = fma(w[ch * kk + ky * p.k + kx], x[(u32(iy) * p.in_w + u32(ix)) * p.c + ch], sum);
        }
    }
    var v = sum + bias[ch];
    if (p.flags & 1u) != 0u {
        v = gelu(v);
    }
    o[idx] = v;
}
