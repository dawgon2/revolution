// nn: multi-head attention, one thread per (group, head, query): softmax(q·kᵀ·scale + bias)·v.
// Groups are windows (windowed attention) or one group (global attention); queries and keys
// may come from different buffers and counts (cross attention). All offsets and strides are in
// vec4 units: query row r of group g starts at q[(g·nq + r)·q_stride + q_off + h·q_head], key /
// value row j at kv[(g·nkv + j)·kv_stride + {k_off, v_off} + h·kv_head], the output at
// o[(g·nq + r)·out_stride + h·out_head]. flags 1: add bias[h][r][j] (a relative position bias,
// [heads][nq][nkv]). Two passes over the keys (max, then exp-sum and the weighted sum of v), so
// nothing per key is stored. HD4 (head dim / 4) is substituted by the host per pipeline.

struct Params {
    total: u32,
    nq: u32,
    nkv: u32,
    heads: u32,
    q_stride: u32,
    kv_stride: u32,
    q_off: u32,
    k_off: u32,
    v_off: u32,
    q_head: u32,
    kv_head: u32,
    out_stride: u32,
    out_head: u32,
    flags: u32,
    scale: f32,
    _p0: u32,
}

const HD4: u32 = 8u;

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> q: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> kv: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> bias: array<f32>;
@group(0) @binding(4) var<storage, read_write> o: array<vec4<f32>>;

fn score(qv: array<vec4<f32>, HD4>, kb: u32, bi: u32) -> f32 {
    var s = 0.0;
    for (var d = 0u; d < HD4; d++) {
        s += dot(qv[d], kv[kb + d]);
    }
    s = s * p.scale;
    if (p.flags & 1u) != 0u {
        s += bias[bi];
    }
    return s;
}

@compute @workgroup_size(64)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 64u + lid.x;
    if idx >= p.total {
        return;
    }
    let i = idx % p.nq;
    let gh = idx / p.nq;
    let h = gh % p.heads;
    let g = gh / p.heads;
    var qv: array<vec4<f32>, HD4>;
    let qb = (g * p.nq + i) * p.q_stride + p.q_off + h * p.q_head;
    for (var d = 0u; d < HD4; d++) {
        qv[d] = q[qb + d];
    }
    let first = g * p.nkv;
    let bb = (h * p.nq + i) * p.nkv;
    var m = -3.0e38;
    for (var j = 0u; j < p.nkv; j++) {
        let kb = (first + j) * p.kv_stride + h * p.kv_head;
        m = max(m, score(qv, kb + p.k_off, bb + j));
    }
    var sum = 0.0;
    var acc: array<vec4<f32>, HD4>;
    for (var d = 0u; d < HD4; d++) {
        acc[d] = vec4<f32>(0.0);
    }
    for (var j = 0u; j < p.nkv; j++) {
        let kb = (first + j) * p.kv_stride + h * p.kv_head;
        let e = exp(score(qv, kb + p.k_off, bb + j) - m);
        sum += e;
        let vb = kb + p.v_off;
        for (var d = 0u; d < HD4; d++) {
            acc[d] = fma(vec4<f32>(e), kv[vb + d], acc[d]);
        }
    }
    let ob = (g * p.nq + i) * p.out_stride + h * p.out_head;
    for (var d = 0u; d < HD4; d++) {
        o[ob + d] = acc[d] / sum;
    }
}
