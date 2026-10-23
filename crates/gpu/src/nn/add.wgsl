// nn: x += src (a residual). mode 0: element for element; mode 1: src holds rows of padded
// ws × ws windows (window-major, nwx windows per row of windows, n = ws² rows each) and is
// added back onto the rh × rw token grid of x (the padding rows are dropped).

struct Params {
    total: u32,
    c: u32,
    rh: u32,
    rw: u32,
    ws: u32,
    nwx: u32,
    n: u32,
    mode: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read_write> x: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    var si = idx;
    if p.mode == 1u {
        let ch = idx % p.c;
        let tok = idx / p.c;
        let ty = tok / p.rw;
        let tx = tok % p.rw;
        let win = (ty / p.ws) * p.nwx + tx / p.ws;
        let row = win * p.n + (ty % p.ws) * p.ws + tx % p.ws;
        si = row * p.c + ch;
    }
    x[idx] = src[si] + x[idx];
}
