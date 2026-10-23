// nn: the patches of a k × k convolution as rows, so the convolution is a matmul:
// col[r][(ky·k + kx)·cin + ci] = x(ci, oy·stride + ky − pad, ox·stride + kx − pad) (0 outside),
// r = oy·out_w + ox. The input is read through strides (sc, sx, sy) so both CHW and HWC work.

struct Params {
    total: u32,
    cin: u32,
    in_h: u32,
    in_w: u32,
    out_w: u32,
    stride: u32,
    sc: u32,
    sx: u32,
    sy: u32,
    kdim: u32,
    k: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read_write> col: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    let row = idx / p.kdim;
    let kidx = idx % p.kdim;
    let tap = kidx / p.cin;
    let ci = kidx % p.cin;
    let ky = tap / p.k;
    let kx = tap % p.k;
    let oy = row / p.out_w;
    let ox = row % p.out_w;
    let iy = i32(oy * p.stride + ky) - i32(p.pad);
    let ix = i32(ox * p.stride + kx) - i32(p.pad);
    var v = 0.0;
    if iy >= 0 && ix >= 0 && u32(iy) < p.in_h && u32(ix) < p.in_w {
        v = x[ci * p.sc + u32(iy) * p.sy + u32(ix) * p.sx];
    }
    col[idx] = v;
}
