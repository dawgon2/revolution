// nn: layer norm over the channels of each row (LayerNorm on channel-last tokens, LayerNorm2d
// on HWC pixels; two-pass mean / variance). One 64-thread workgroup per row.
// mode 1 gathers the rows of zero-padded ws × ws windows: output row = window·n + t (n = ws²,
// nwx windows per row of windows) from token (y, x) of the rh × rw grid; a padding token is a
// zero vector (it normalises to `beta`, as when the padding is added before the norm).
// flags 1: write channel-major (out[ch·rows + row], a CHW image); 2: a padding token is written
// as zeros instead (the padding is added after the norm, as SAM 2's Hiera).

struct Params {
    rows: u32,
    c: u32,
    mode: u32,
    flags: u32,
    rh: u32,
    rw: u32,
    ws: u32,
    nwx: u32,
    n: u32,
    eps: f32,
    _p0: u32,
    _p1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read> gamma: array<f32>;
@group(0) @binding(3) var<storage, read> beta: array<f32>;
@group(0) @binding(4) var<storage, read_write> o: array<f32>;

var<workgroup> red: array<f32, 64>;

fn reduce(v: f32, l: u32) -> f32 {
    red[l] = v;
    workgroupBarrier();
    for (var s = 32u; s > 0u; s = s / 2u) {
        if l < s {
            red[l] = red[l] + red[l + s];
        }
        workgroupBarrier();
    }
    let total = red[0];
    workgroupBarrier();
    return total;
}

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let row = wid.y * nwg.x + wid.x;
    if row >= p.rows {
        return;
    }
    let l = lid.x;
    var valid = true;
    var src = row;
    if p.mode == 1u {
        let win = row / p.n;
        let t = row % p.n;
        let y = (win / p.nwx) * p.ws + t / p.ws;
        let xx = (win % p.nwx) * p.ws + t % p.ws;
        valid = y < p.rh && xx < p.rw;
        src = y * p.rw + xx;
    }
    var sum = 0.0;
    if valid {
        for (var ch = l; ch < p.c; ch += 64u) {
            sum += x[src * p.c + ch];
        }
    }
    let mean = reduce(sum, l) / f32(p.c);
    var sq = 0.0;
    if valid {
        for (var ch = l; ch < p.c; ch += 64u) {
            let d = x[src * p.c + ch] - mean;
            sq += d * d;
        }
    }
    let vr = reduce(sq, l) / f32(p.c);
    let den = sqrt(vr + p.eps);
    for (var ch = l; ch < p.c; ch += 64u) {
        var v = 0.0;
        if valid {
            v = x[src * p.c + ch];
        }
        var y = (v - mean) / den * gamma[ch] + beta[ch];
        if !valid && (p.flags & 2u) != 0u {
            y = 0.0;
        }
        if (p.flags & 1u) != 0u {
            o[ch * p.rows + row] = y;
        } else {
            o[row * p.c + ch] = y;
        }
    }
}
