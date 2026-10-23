// nn: o[i] = alpha · a[i] + beta · b[i % blen] (fork G3.4): a scaled sum, with b broadcast
// over rows when it is shorter (a per-channel vector).

struct Params {
    total: u32,
    blen: u32,
    alpha: f32,
    beta: f32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> o: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    o[idx] = p.alpha * a[idx] + p.beta * b[idx % p.blen];
}
