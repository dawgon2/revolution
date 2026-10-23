// nn: softmax over each row of `n` values, in place (max-subtracted). One 64-thread workgroup
// per row.

struct Params {
    rows: u32,
    n: u32,
    _p0: u32,
    _p1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> x: array<f32>;

var<workgroup> red: array<f32, 64>;

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let row = wid.y * nwg.x + wid.x;
    if row >= p.rows {
        return;
    }
    let l = lid.x;
    let base = row * p.n;
    var m = -3.0e38;
    for (var j = l; j < p.n; j += 64u) {
        m = max(m, x[base + j]);
    }
    red[l] = m;
    workgroupBarrier();
    for (var s = 32u; s > 0u; s = s / 2u) {
        if l < s {
            red[l] = max(red[l], red[l + s]);
        }
        workgroupBarrier();
    }
    let mx = red[0];
    workgroupBarrier();
    var sum = 0.0;
    for (var j = l; j < p.n; j += 64u) {
        sum += exp(x[base + j] - mx);
    }
    red[l] = sum;
    workgroupBarrier();
    for (var s = 32u; s > 0u; s = s / 2u) {
        if l < s {
            red[l] = red[l] + red[l + s];
        }
        workgroupBarrier();
    }
    let total = red[0];
    for (var j = l; j < p.n; j += 64u) {
        x[base + j] = exp(x[base + j] - mx) / total;
    }
}
