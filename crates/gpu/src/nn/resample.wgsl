// nn: pooling and upsampling of HWC images (h × w × c in, oh × ow × c out).
// mode 0: max pool, k × k windows with stride k (no padding; PyTorch MaxPool2d(k, k));
// 1: average pool, same windows; 2: nearest (PyTorch `nearest`: src = floor(dst · in / out));
// 3: bilinear, half-pixel centres (PyTorch `bilinear`, align_corners = false).

struct Params {
    total: u32,
    c: u32,
    h: u32,
    w: u32,
    oh: u32,
    ow: u32,
    mode: u32,
    k: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read_write> o: array<f32>;

fn at(y: u32, xx: u32, ch: u32) -> f32 {
    return x[(y * p.w + xx) * p.c + ch];
}

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_id) lid: vec3<u32>, @builtin(workgroup_id) wid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let idx = (wid.y * nwg.x + wid.x) * 256u + lid.x;
    if idx >= p.total {
        return;
    }
    let ch = idx % p.c;
    let pix = idx / p.c;
    let oy = pix / p.ow;
    let ox = pix % p.ow;
    var v = 0.0;
    if p.mode <= 1u {
        var m = -3.0e38;
        var s = 0.0;
        for (var dy = 0u; dy < p.k; dy++) {
            for (var dx = 0u; dx < p.k; dx++) {
                let y = min(oy * p.k + dy, p.h - 1u);
                let xx = min(ox * p.k + dx, p.w - 1u);
                let a = at(y, xx, ch);
                m = max(m, a);
                s += a;
            }
        }
        v = select(s / f32(max(p.k * p.k, 1u)), m, p.mode == 0u);
    } else if p.mode == 2u {
        let y = min(oy * p.h / p.oh, p.h - 1u);
        let xx = min(ox * p.w / p.ow, p.w - 1u);
        v = at(y, xx, ch);
    } else {
        let sy = max((f32(oy) + 0.5) * f32(p.h) / f32(p.oh) - 0.5, 0.0);
        let sx = max((f32(ox) + 0.5) * f32(p.w) / f32(p.ow) - 0.5, 0.0);
        let y0 = min(u32(sy), p.h - 1u);
        let x0 = min(u32(sx), p.w - 1u);
        let y1 = min(y0 + 1u, p.h - 1u);
        let x1 = min(x0 + 1u, p.w - 1u);
        let fy = sy - f32(y0);
        let fx = sx - f32(x0);
        let top = at(y0, x0, ch) * (1.0 - fx) + at(y0, x1, ch) * fx;
        let bot = at(y1, x0, ch) * (1.0 - fx) + at(y1, x1, ch) * fx;
        v = top * (1.0 - fy) + bot * fy;
    }
    o[idx] = v;
}
