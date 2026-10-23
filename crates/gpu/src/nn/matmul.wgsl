// nn: o[m][n] = epilogue(Σ_k a[m][k] · w[n][k] + bias[n]). `w` is a PyTorch Linear / 1×1
// conv weight ([out][in]), or [in][out] with flag 8; im2col turns k×k convs into this too.
// Epilogue flags: 1 = GELU (tanh form, as candle's `gelu`), 2 = add into the existing o (a
// residual), 4 = GELU after that add, 8 = w is [k][n].
//
// Tiling: a 256-thread workgroup computes a 64 × 64 tile walking K in steps of 16, staging
// both operands in workgroup memory (k-major). Each thread owns rows ty + 16·i and columns
// tx + 16·j (i, j < 4), so neighbouring threads write neighbouring columns.
// Shape: the host guarantees m·k, n·k, m·n within the bindings and below 2³².

struct Params {
    m: u32,
    n: u32,
    k: u32,
    flags: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> w: array<f32>;
@group(0) @binding(3) var<storage, read> bias: array<f32>;
@group(0) @binding(4) var<storage, read_write> o: array<f32>;

var<workgroup> sa: array<array<f32, 64>, 16>;
var<workgroup> sb: array<array<f32, 64>, 16>;

fn gelu(x: f32) -> f32 {
    let inner = 0.7978845608028654 * x * (1.0 + 0.044715 * x * x);
    return 0.5 * x * (1.0 + tanh(clamp(inner, -15.0, 15.0)));
}

fn store(gm: u32, gn: u32, v: f32) {
    if gm >= p.m || gn >= p.n {
        return;
    }
    var x = v + bias[gn];
    if (p.flags & 1u) != 0u {
        x = gelu(x);
    }
    let oi = gm * p.n + gn;
    if (p.flags & 2u) != 0u {
        x = o[oi] + x;
    }
    if (p.flags & 4u) != 0u {
        x = gelu(x);
    }
    o[oi] = x;
}

fn store_row(gm: u32, n0: u32, v: vec4<f32>) {
    store(gm, n0, v.x);
    store(gm, n0 + 16u, v.y);
    store(gm, n0 + 32u, v.z);
    store(gm, n0 + 48u, v.w);
}

@compute @workgroup_size(16, 16)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>) {
    let tx = lid.x;
    let ty = lid.y;
    let t = ty * 16u + tx;
    let m0 = wid.y * 64u;
    let n0 = wid.x * 64u;
    var acc0 = vec4<f32>(0.0);
    var acc1 = vec4<f32>(0.0);
    var acc2 = vec4<f32>(0.0);
    var acc3 = vec4<f32>(0.0);
    let steps = (p.k + 15u) / 16u;
    for (var s = 0u; s < steps; s++) {
        let k0 = s * 16u;
        for (var e = 0u; e < 4u; e++) {
            let idx = t + e * 256u;
            let r = idx / 16u;
            let kk = idx % 16u;
            let gk = k0 + kk;
            let gm = m0 + r;
            var va = 0.0;
            if gm < p.m && gk < p.k {
                va = a[gm * p.k + gk];
            }
            sa[kk][r] = va;
            let gn = n0 + r;
            var vb = 0.0;
            if gn < p.n && gk < p.k {
                if (p.flags & 8u) != 0u {
                    vb = w[gk * p.n + gn];
                } else {
                    vb = w[gn * p.k + gk];
                }
            }
            sb[kk][r] = vb;
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
    store_row(m0 + ty, n0 + tx, acc0);
    store_row(m0 + ty + 16u, n0 + tx, acc1);
    store_row(m0 + ty + 32u, n0 + tx, acc2);
    store_row(m0 + ty + 48u, n0 + tx, acc3);
}
