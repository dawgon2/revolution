// nn: rotary position encoding in place (SAM 2's 2-D axial RoPE; fork G3.4). Row r's pairs
// (2j, 2j+1) at x[r·stride + off + 2j] turn by the table angle of token r % period:
// (a, b) → (a·cos − b·sin, a·sin + b·cos), tables [period][pairs].

struct Params {
    total: u32,
    pairs: u32,
    stride: u32,
    off: u32,
    period: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> x: array<f32>;
@group(0) @binding(2) var<storage, read> cs: array<f32>;
@group(0) @binding(3) var<storage, read> sn: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    let j = idx % p.pairs;
    let r = idx / p.pairs;
    let ti = (r % p.period) * p.pairs + j;
    let c = cs[ti];
    let s = sn[ti];
    let i = r * p.stride + p.off + 2u * j;
    let a = x[i];
    let b = x[i + 1u];
    x[i] = a * c - b * s;
    x[i + 1u] = a * s + b * c;
}
