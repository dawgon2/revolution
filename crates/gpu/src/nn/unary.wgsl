// nn: an activation in place. op 0: GELU, tanh form (candle `gelu`); 1: GELU, exact erf form
// (PyTorch `nn.GELU()`, candle `gelu_erf`; erf to ~1.2e-7, Numerical Recipes' erfc); 2: ReLU;
// 3: SiLU; 4: sigmoid.

struct Params {
    total: u32,
    op: u32,
    _p0: u32,
    _p1: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> x: array<f32>;

fn erfc_pos(z: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277))))))));
    return t * exp(poly);
}

fn erf(v: f32) -> f32 {
    let e = 1.0 - erfc_pos(abs(v));
    return select(-e, e, v >= 0.0);
}

fn sigmoid(v: f32) -> f32 {
    return 1.0 / (1.0 + exp(-clamp(v, -80.0, 80.0)));
}

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    let v = x[idx];
    var y = v;
    switch p.op {
        case 0u: {
            let inner = 0.7978845608028654 * v * (1.0 + 0.044715 * v * v);
            y = 0.5 * v * (1.0 + tanh(clamp(inner, -15.0, 15.0)));
        }
        case 1u: {
            y = 0.5 * v * (1.0 + erf(v * 0.7071067811865476));
        }
        case 2u: {
            y = max(v, 0.0);
        }
        case 3u: {
            y = v * sigmoid(v);
        }
        case 4u: {
            y = sigmoid(v);
        }
        default: {}
    }
    x[idx] = y;
}
