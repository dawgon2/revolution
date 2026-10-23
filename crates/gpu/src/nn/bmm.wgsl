// nn: strided, batched matmul (SAM 2's Hiera / memory attention; fork G3.4).
// For batch b (workgroup z): o[b][m][n] = epilogue(alpha · Σ_k a[b][m][k] · w[b][·] + bias[n]),
// with every operand read through an offset, a row stride and a batch stride (floats), so
// heads and query chunks are addressed in place (q·kᵀ and p·v straight out of a fused qkv
// buffer). w is [n][k] (PyTorch Linear / keys) or, with flag 8, [k][n] (values).
// Flags: 1 GELU (tanh), 2 add into o (residual), 4 GELU (tanh) after the add, 8 w is [k][n],
// 16 GELU (erf, PyTorch nn.GELU), 32 ReLU, 64 add bias.
// Tiling: a 64 × 64 output tile per 256-thread workgroup walking K in steps of 16, both
// operands staged in workgroup memory (k-major); each thread owns rows ty + 16·i and columns
// tx + 16·j (i, j < 4). The host checks every index stays below 2³² and inside its binding.

struct Params {
    m: u32,
    n: u32,
    k: u32,
    flags: u32,
    a_off: u32,
    a_ld: u32,
    a_bs: u32,
    w_off: u32,
    w_ld: u32,
    w_bs: u32,
    o_off: u32,
    o_ld: u32,
    o_bs: u32,
    alpha: f32,
    _p0: u32,
    _p1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> w: array<f32>;
@group(0) @binding(3) var<storage, read> bias: array<f32>;
@group(0) @binding(4) var<storage, read_write> o: array<f32>;

var<workgroup> sa: array<array<f32, 64>, 16>;
var<workgroup> sb: array<array<f32, 64>, 16>;

fn erfc_pos(z: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277))))))));
    return t * exp(poly);
}

fn gelu_erf(v: f32) -> f32 {
    let z = v * 0.7071067811865476;
    let e = 1.0 - erfc_pos(abs(z));
    return 0.5 * v * (1.0 + select(-e, e, z >= 0.0));
}

fn gelu_tanh(x: f32) -> f32 {
    let inner = 0.7978845608028654 * x * (1.0 + 0.044715 * x * x);
    return 0.5 * x * (1.0 + tanh(clamp(inner, -15.0, 15.0)));
}

fn store(b: u32, gm: u32, gn: u32, v: f32) {
    if gm >= p.m || gn >= p.n {
        return;
    }
    var x = v * p.alpha;
    if (p.flags & 64u) != 0u {
        x += bias[gn];
    }
    if (p.flags & 1u) != 0u {
        x = gelu_tanh(x);
    }
    if (p.flags & 16u) != 0u {
        x = gelu_erf(x);
    }
    if (p.flags & 32u) != 0u {
        x = max(x, 0.0);
    }
    let oi = p.o_off + b * p.o_bs + gm * p.o_ld + gn;
    if (p.flags & 2u) != 0u {
        x = o[oi] + x;
    }
    if (p.flags & 4u) != 0u {
        x = gelu_tanh(x);
    }
    o[oi] = x;
}

fn store_row(b: u32, gm: u32, n0: u32, v: vec4<f32>) {
    store(b, gm, n0, v.x);
    store(b, gm, n0 + 16u, v.y);
    store(b, gm, n0 + 32u, v.z);
    store(b, gm, n0 + 48u, v.w);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let tx = lid.x;
    let ty = lid.y;
    let t = ty * 16u + tx;
    let b = wid.z;
    let m0 = wid.y * 64u;
    let n0 = wid.x * 64u;
    let abase = p.a_off + b * p.a_bs;
    let wbase = p.w_off + b * p.w_bs;
    let kn = (p.flags & 8u) != 0u;
    var acc0 = vec4<f32>(0.0);
    var acc1 = vec4<f32>(0.0);
    var acc2 = vec4<f32>(0.0);
    var acc3 = vec4<f32>(0.0);
    let steps = (p.k + 15u) / 16u;
    for (var s = 0u; s < steps; s++) {
        let k0 = s * 16u;
        for (var e = 0u; e < 4u; e++) {
            let idx = t + e * 256u;
            // a: consecutive threads read consecutive k
            let r = idx / 16u;
            let kk = idx % 16u;
            let gk = k0 + kk;
            let gm = m0 + r;
            var va = 0.0;
            if gm < p.m && gk < p.k {
                va = a[abase + gm * p.a_ld + gk];
            }
            sa[kk][r] = va;
            var vb = 0.0;
            if kn {
                // w[k][n]: consecutive threads read consecutive n
                let rn = idx % 64u;
                let kn_k = idx / 64u;
                let gk2 = k0 + kn_k;
                let gn = n0 + rn;
                if gn < p.n && gk2 < p.k {
                    vb = w[wbase + gk2 * p.w_ld + gn];
                }
                sb[kn_k][rn] = vb;
            } else {
                let gn = n0 + r;
                if gn < p.n && gk < p.k {
                    vb = w[wbase + gn * p.w_ld + gk];
                }
                sb[kk][r] = vb;
            }
        }
        workgroupBarrier();
        for (var kk = 0u; kk < 16u; kk++) {
            let av = vec4<f32>(sa[kk][ty], sa[kk][ty + 16u], sa[kk][ty + 32u], sa[kk][ty + 48u]);
            let bv = vec4<f32>(sb[kk][tx], sb[kk][tx + 16u], sb[kk][tx + 32u], sb[kk][tx + 48u]);
            acc0 = fma(vec4<f32>(av.x), bv, acc0);
            acc1 = fma(vec4<f32>(av.y), bv, acc1);
            acc2 = fma(vec4<f32>(av.z), bv, acc2);
            acc3 = fma(vec4<f32>(av.w), bv, acc3);
        }
        workgroupBarrier();
    }
    store_row(b, m0 + ty, n0 + tx, acc0);
    store_row(b, m0 + ty + 16u, n0 + tx, acc1);
    store_row(b, m0 + ty + 32u, n0 + tx, acc2);
    store_row(b, m0 + ty + 48u, n0 + tx, acc3);
}
