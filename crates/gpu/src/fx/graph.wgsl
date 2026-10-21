// The passes of a branched colour grade (fork C2, `docs/fork/DESIGN-C2.md` §2–3) that are not an
// existing effect: the GPU twin of `filmcraft_render::grade::execute_graph_cpu`, same f32
// arithmetic per pixel. Pictures are premultiplied on the grading signal; keys are grey pictures
// (the key in R, G and B, alpha 1). Multi-input passes (mix, combine, key mix) run one input at a
// time, accumulating in `aux`. The module is this file plus the blend helpers of `composite.wgsl`
// (the layer blend, so Over is Merge's maths) and the window coverage of `colorops.wgsl`
// (`fx/graph.rs` `module_source`).

struct U {
    code: u32,
    w: u32,
    h: u32,
    pad: u32,
    i1: vec4<u32>,
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    p4: vec4<f32>,
    p5: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var aux: texture_2d<f32>;
// the window program (`mask_coverage`)
@group(0) @binding(4) var<storage, read> prog: array<f32>;
@group(0) @binding(5) var aux2: texture_2d<f32>;

const G_COPY: u32 = 1u;
const G_FILL: u32 = 2u;
const G_MATTE: u32 = 3u;
const G_KEY_IN: u32 = 4u;
const G_LIMIT: u32 = 5u;
const G_MIX: u32 = 6u;
const G_OVER: u32 = 7u;
const G_CHANNEL: u32 = 8u;
const G_COMBINE_INIT: u32 = 9u;
const G_COMBINE_SET: u32 = 10u;
const G_KEY_MIX: u32 = 11u;

fn at(i: u32) -> f32 {
    return prog[min(i, arrayLength(&prog) - 1u)];
}

// A key value clamped to 0…1; a non-finite one is 0 (`graph.rs` `finite_key`; by its bits, as
// a compiler may assume floats are finite).
fn finite_key(k: f32) -> f32 {
    if ((bitcast<u32>(k) & 0x7f800000u) == 0x7f800000u) {
        return 0.0;
    }
    return clamp(k, 0.0, 1.0);
}

fn grey(v: f32) -> vec4<f32> {
    return vec4(v, v, v, 1.0);
}

// `blend::composite_at` for one pixel: `s` (opacity applied here) over `d` with `mode`.
fn over(s: vec4<f32>, d: vec4<f32>, mode: u32, opacity: f32, x: u32, y: u32) -> vec4<f32> {
    var sp = s * opacity;
    let sa0 = sp.a;
    if (sa0 <= 0.0) {
        return d;
    }
    if (mode == 1u) {
        if (hash2(x, y) >= sa0) {
            return d;
        }
        sp = vec4(sp.rgb / sa0, 1.0);
    }
    let da = d.a;
    if (mode == 0u || mode == 1u || da <= 0.0) {
        return sp + d * (1.0 - sp.a);
    }
    let sa = sp.a;
    let cs = linear_to_srgb(sp.rgb / sa);
    let cb = linear_to_srgb(d.rgb / da);
    let mixed = srgb_to_linear(clamp(blend_rgb(mode, cb, cs), vec3(0.0), vec3(1.0)));
    return vec4(sp.rgb * (1.0 - da) + d.rgb * (1.0 - sa) + sa * da * mixed, sa + da - sa * da);
}

@compute @workgroup_size(16, 16)
fn graph_px(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u.w || id.y >= u.h) {
        return;
    }
    let p = vec2<i32>(id.xy);
    var out = vec4(0.0);
    switch u.code {
        case G_COPY: {
            out = textureLoad(src, p, 0);
        }
        case G_FILL: {
            out = u.p0;
        }
        // a node's key: i1.x 0 = everywhere (1), 1 = `src`'s red clamped, 2 = as it is (after its
        // blur); then inverted (i1.y) and times the windows' coverage (i1.z)
        case G_MATTE: {
            var v = 1.0;
            if (u.i1.x == 1u) {
                v = finite_key(textureLoad(src, p, 0).x);
            } else if (u.i1.x == 2u) {
                v = textureLoad(src, p, 0).x;
            }
            if (u.i1.y != 0u) {
                v = 1.0 - v;
            }
            if (u.i1.z != 0u) {
                v = v * mask_coverage(vec2<f32>(f32(id.x) + 0.5, f32(id.y) + 0.5));
            }
            out = grey(v);
        }
        // `src` (a picture) times the key in `aux`: inverted (i1.x), × p0.x + p0.y, clamped
        case G_KEY_IN: {
            let d = textureLoad(src, p, 0);
            var k = finite_key(textureLoad(aux, p, 0).x);
            if (u.i1.x != 0u) {
                k = 1.0 - k;
            }
            let f = clamp(k * u.p0.x + u.p0.y, 0.0, 1.0);
            out = vec4(d.rgb * f, d.a);
        }
        // the corrected `src` mixed with the original `aux` by the key in `aux2`
        case G_LIMIT: {
            let d = textureLoad(src, p, 0);
            let o = textureLoad(aux, p, 0);
            let m = finite_key(textureLoad(aux2, p, 0).x);
            out = o + (d - o) * m;
        }
        // the sum so far (`aux`; i1.x: none yet) plus `src` × p0.x, alpha × p0.y (the share)
        case G_MIX: {
            let s = textureLoad(src, p, 0);
            var acc = vec4(0.0);
            if (u.i1.x == 0u) {
                acc = textureLoad(aux, p, 0);
            }
            out = vec4(acc.rgb + s.rgb * u.p0.x, acc.a + s.a * u.p0.y);
        }
        // `src` over `aux` with blend mode i1.x and opacity p0.x
        case G_OVER: {
            out = over(textureLoad(src, p, 0), textureLoad(aux, p, 0), u.i1.x, u.p0.x, id.x, id.y);
        }
        // channel i1.x of `src` as a grey picture with `src`'s alpha
        case G_CHANNEL: {
            let s = textureLoad(src, p, 0);
            var v = s.z;
            if (u.i1.x == 0u) {
                v = s.x;
            } else if (u.i1.x == 1u) {
                v = s.y;
            }
            out = vec4(v, v, v, s.a);
        }
        // black with the alpha of `src` (i1.x: there is one)
        case G_COMBINE_INIT: {
            if (u.i1.x != 0u) {
                out = vec4(0.0, 0.0, 0.0, textureLoad(src, p, 0).a);
            }
        }
        // `aux` with channel i1.x taken from `src`
        case G_COMBINE_SET: {
            let s = textureLoad(src, p, 0);
            out = textureLoad(aux, p, 0);
            if (u.i1.x == 0u) {
                out.x = s.x;
            } else if (u.i1.x == 1u) {
                out.y = s.y;
            } else {
                out.z = s.z;
            }
        }
        // the key of `src` (inverted when i1.z) combined with the keys so far in `aux` by mode
        // i1.y (0 add, 1 subtract, 2 intersect); i1.x: the first key
        case G_KEY_MIX: {
            var k = finite_key(textureLoad(src, p, 0).x);
            if (u.i1.z != 0u) {
                k = 1.0 - k;
            }
            var v = k;
            if (u.i1.x == 0u) {
                let a = textureLoad(aux, p, 0).x;
                if (u.i1.y == 1u) {
                    v = max(a - k, 0.0);
                } else if (u.i1.y == 2u) {
                    v = a * k;
                } else {
                    v = min(a + k, 1.0);
                }
            }
            out = grey(v);
        }
        default: {
            out = textureLoad(src, p, 0);
        }
    }
    textureStore(dst, p, out);
}
