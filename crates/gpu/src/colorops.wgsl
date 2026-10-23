// A chain of per-pixel colour operations in one pass (fork ADR 0003), the GPU twin of
// `filmcraft_color::ops::ColorOp::apply`: same order, same f32 arithmetic. The program is a flat
// array of f32 written by `colorops.rs`: per op its code, then its parameters (tables inline).
// Pixels are premultiplied; like `Image::map_rgb` the chain sees straight colour, and pixels with
// alpha ≤ 1e-6 pass unchanged.

struct U {
    code: u32,
    w: u32,
    h: u32,
    pad: u32,
    // i1.x: program length (f32 values)
    i1: vec4<u32>,
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba32float, write>;
@group(0) @binding(4) var<storage, read> prog: array<f32>;
// the image a masked chain corrects (`chain_masked`; its mask is `src`)
@group(0) @binding(3) var aux: texture_2d<f32>;

// the mask register (`ColorOp::apply_px`)
var<private> mask_reg: f32 = 1.0;
// the stash register (CO_STASH / CO_MIX_STASH; the chain's input until a CO_STASH)
var<private> stash_reg: vec3<f32> = vec3(0.0);

const CO_MATRIX: u32 = 1u;
const CO_DECODE: u32 = 2u;
const CO_ENCODE: u32 = 3u;
const CO_TABLE: u32 = 4u;
const CO_LGGO: u32 = 5u;
const CO_SATURATION: u32 = 6u;
const CO_HLG_OOTF: u32 = 7u;
const CO_TONE_MAP: u32 = 8u;
const CO_GAMUT_COMPRESS: u32 = 9u;
const CO_DESATURATE: u32 = 10u;
const CO_CLAMP: u32 = 11u;
const CO_TONE_REGIONS: u32 = 12u;
const CO_CONTRAST: u32 = 13u;
const CO_SPECULAR: u32 = 14u;
const CO_SPLIT_TONE: u32 = 15u;
const CO_VIGNETTE: u32 = 16u;

// Lumetri Contrast of one channel (`ColorOp::Contrast`)
fn contrast(v: f32, k: f32, keep: bool) -> f32 {
    if (keep && v > 1.0) {
        return v;
    }
    let q = clamp(v, 0.0, 1.0);
    let s = q * q * (3.0 - 2.0 * q);
    if (k >= 1.0) {
        return q + (s - q) * (k - 1.0);
    }
    return 0.5 + (q - 0.5) * k;
}

const CO_CURVE: u32 = 17u;
const CO_WHEELS: u32 = 18u;
const CO_HSL_CURVES: u32 = 19u;
const CO_LUT: u32 = 20u;
const CO_LOOK: u32 = 21u;
const CO_HSL_KEY: u32 = 22u;
const CO_HSL_CORRECT: u32 = 23u;
const CO_GRAIN: u32 = 24u;
const CO_STASH: u32 = 25u;
const CO_MIX_STASH: u32 = 26u;
// GPU-only codes (`fx.rs` `fused_steps`, fork X422 2b.2): an HSL Secondary without blur or denoise
// as one chain, key then correct: KEY_BEGIN keeps the colour and starts the mask as `chain_key`
// does, KEY_END brings the colour back for the correct ops (the mask stays, as `chain_masked`
// reads it).
const CO_KEY_BEGIN: u32 = 27u;
const CO_KEY_END: u32 = 28u;
// colour warper (fork G4.1): `filmcraft_color::warp::Warp2D`
const CO_WARP: u32 = 29u;

// HDR zone wheels (fork G4.2; 29 is the colour warper's, G4.1)
const CO_ZONE_WHEELS: u32 = 30u;

// `ops::zone_weight`: smoothstep across `edge ± falloff / 2`, reversed for a dark zone.
fn zone_weight(dark: bool, s: f32, edge: f32, falloff: f32) -> f32 {
    let f = max(falloff, 0.05);
    let a = clamp((s - edge) / f + 0.5, 0.0, 1.0);
    let u = a * a * (3.0 - 2.0 * a);
    if (dark) {
        return 1.0 - u;
    }
    return u;
}

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let r = c.x; let g = c.y; let b = c.z;
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let l = (mx + mn) / 2.0;
    if abs(mx - mn) < 1e-7 {
        return vec3(0.0, 0.0, l);
    }
    let d = mx - mn;
    var s: f32;
    if l > 0.5 {
        s = d / (2.0 - mx - mn);
    } else {
        s = d / (mx + mn);
    }
    var h: f32;
    if mx == r {
        h = (g - b) / d + select(0.0, 6.0, g < b);
    } else if mx == g {
        h = (b - r) / d + 2.0;
    } else {
        h = (r - g) / d + 4.0;
    }
    return vec3(h / 6.0, s, l);
}

fn fract_euclid(x: f32) -> f32 {
    return x - floor(x);
}

fn hsl_channel(p: f32, q: f32, t0: f32) -> f32 {
    let t = fract_euclid(t0);
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 0.5 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    if s <= 0.0 {
        return vec3(l);
    }
    var q: f32;
    if l < 0.5 {
        q = l * (1.0 + s);
    } else {
        q = l + s - l * s;
    }
    let p = 2.0 * l - q;
    return vec3(hsl_channel(p, q, h + 1.0 / 3.0), hsl_channel(p, q, h), hsl_channel(p, q, h - 1.0 / 3.0));
}

// `within_unit`: the factor that brings a signal into 0…1 (1: clamp instead)
fn unit_scale(c: vec3<f32>, hdr: bool) -> f32 {
    if (hdr) {
        return max(max(c.x, max(c.y, c.z)), 1.0);
    }
    return 1.0;
}

fn to_unit(c: vec3<f32>, hdr: bool, m: f32) -> vec3<f32> {
    if (hdr) {
        return max(c / m, vec3(0.0));
    }
    return clamp(c, vec3(0.0), vec3(1.0));
}

// `Table::eval_curve` of the table at `base` (lo, hi, n, values)
fn curve_at(base: u32, x: f32, hdr: bool) -> f32 {
    let lo = at(base);
    let hi = at(base + 1u);
    let n = u32(at(base + 2u));
    if (hdr && x > 1.0) {
        return table(base + 3u, n, lo, hi, 1.0) + (x - 1.0);
    }
    return table(base + 3u, n, lo, hi, clamp(x, 0.0, 1.0));
}

// the end of a table at `base` in the program
fn table_end(base: u32) -> u32 {
    return base + 3u + u32(at(base + 2u));
}

fn s_curve(v: f32, k: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0);
    return x + (x * x * (3.0 - 2.0 * x) - x) * k;
}

// `ops::apply_look`
fn look(n: u32, c: vec3<f32>) -> vec3<f32> {
    let l = 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
    let g = vec3(l);
    switch n {
        case 1u: {
            let sh = vec3(0.0, 0.08, 0.1);
            let hi = vec3(0.1, 0.04, -0.06);
            let d = c + sh * (1.0 - l) + hi * l;
            return vec3(s_curve(d.x, 0.35), s_curve(d.y, 0.35), s_curve(d.z, 0.35));
        }
        case 2u: {
            let d = vec3(c.x * 1.06 + 0.02, c.y * 1.0 + 0.01, c.z * 0.9);
            return (d + (g - d) * 0.15) * 0.94 + vec3(0.04);
        }
        case 3u: {
            let d = vec3(c.x * 0.9, c.y * 0.98, c.z * 1.1 + 0.02);
            return vec3(s_curve(d.x, 0.2), s_curve(d.y, 0.2), s_curve(d.z, 0.2));
        }
        case 4u: {
            let d = c + (g - c) * 0.55;
            return vec3(s_curve(d.x, 0.6), s_curve(d.y, 0.6), s_curve(d.z, 0.6));
        }
        case 5u: {
            let d = c + (g - c) * 0.25;
            return vec3(0.08) + d * 0.84;
        }
        case 6u: {
            return vec3(s_curve(l, 0.3));
        }
        case 7u: {
            let d = vec3(c.x * 1.1 + 0.03, c.y * 1.02 + 0.01, c.z * 0.82);
            return vec3(s_curve(d.x, 0.15), s_curve(d.y, 0.15), s_curve(d.z, 0.15));
        }
        case 8u: {
            return vec3(c.x * 0.75, c.y * 0.85, c.z * 1.15) * 0.8;
        }
        default: {
            return c;
        }
    }
}

fn lut_rgb(i: u32) -> vec3<f32> {
    return vec3(at(i), at(i + 1u), at(i + 2u));
}

// `Lut1d::apply` of the shaper at `base` (n, dmin, dmax, data)
fn lut1d(base: u32, c: vec3<f32>) -> vec3<f32> {
    let n = u32(at(base)) - 1u;
    let data = base + 7u;
    var o = c;
    for (var k = 0u; k < 3u; k++) {
        let lo = at(base + 1u + k);
        let hi = at(base + 4u + k);
        let t = clamp((c[k] - lo) / (hi - lo), 0.0, 1.0) * f32(n);
        let i = min(u32(t), max(n, 1u) - 1u);
        let f = t - f32(i);
        let a = at(data + i * 3u + k);
        let b = at(data + min(i + 1u, n) * 3u + k);
        o[k] = a + (b - a) * f;
    }
    return o;
}

// `Lut3d::apply` (tetrahedral) of the cube at `base` (size, dmin, dmax, data)
fn lut3d(base: u32, rgb: vec3<f32>) -> vec3<f32> {
    let n = u32(at(base));
    let data = base + 7u;
    var q: vec3<f32>;
    for (var k = 0u; k < 3u; k++) {
        let lo = at(base + 1u + k);
        let hi = at(base + 4u + k);
        q[k] = clamp((rgb[k] - lo) / (hi - lo), 0.0, 1.0) * f32(n - 1u);
    }
    let r0 = min(u32(q.x), n - 2u);
    let g0 = min(u32(q.y), n - 2u);
    let b0 = min(u32(q.z), n - 2u);
    let fr = q.x - f32(r0);
    let fg = q.y - f32(g0);
    let fb = q.z - f32(b0);
    let i000 = data + (r0 + g0 * n + b0 * n * n) * 3u;
    let dr = 3u;
    let dg = n * 3u;
    let db = n * n * 3u;
    let c000 = lut_rgb(i000);
    let c111 = lut_rgb(i000 + dr + dg + db);
    var ca: vec3<f32>;
    var cb: vec3<f32>;
    var w: vec4<f32>;
    if (fr > fg) {
        if (fg > fb) {
            ca = lut_rgb(i000 + dr); cb = lut_rgb(i000 + dr + dg); w = vec4(1.0 - fr, fr - fg, fg - fb, fb);
        } else if (fr > fb) {
            ca = lut_rgb(i000 + dr); cb = lut_rgb(i000 + dr + db); w = vec4(1.0 - fr, fr - fb, fb - fg, fg);
        } else {
            ca = lut_rgb(i000 + db); cb = lut_rgb(i000 + dr + db); w = vec4(1.0 - fb, fb - fr, fr - fg, fg);
        }
    } else if (fb > fg) {
        ca = lut_rgb(i000 + db); cb = lut_rgb(i000 + dg + db); w = vec4(1.0 - fb, fb - fg, fg - fr, fr);
    } else if (fb > fr) {
        ca = lut_rgb(i000 + dg); cb = lut_rgb(i000 + dg + db); w = vec4(1.0 - fg, fg - fb, fb - fr, fr);
    } else {
        ca = lut_rgb(i000 + dg); cb = lut_rgb(i000 + dr + dg); w = vec4(1.0 - fg, fg - fr, fr - fb, fb);
    }
    return w.x * c000 + w.y * ca + w.z * cb + w.w * c111;
}

fn specular(v: f32, gain: f32) -> f32 {
    if (v > 1.0) {
        return 1.0 + (v - 1.0) * gain;
    }
    return v;
}

// `transform::gamut_compress`
fn gamut_compress(c: vec3<f32>) -> vec3<f32> {
    let th = 0.8;
    let lim = 1.25;
    let p = 1.2;
    let ach = max(c.x, max(c.y, c.z));
    if (ach <= 1e-9) {
        return max(c, vec3(0.0));
    }
    let s = (lim - th) / pow(pow((lim - th) / (1.0 - th), p) - 1.0, 1.0 / p);
    var o = c;
    for (var k = 0u; k < 3u; k++) {
        let v = c[k];
        let d = (ach - v) / ach;
        if (d >= th) {
            let x = (d - th) / s;
            let dc = th + s * x / pow(1.0 + pow(x, p), 1.0 / p);
            o[k] = ach - dc * ach;
        }
    }
    return o;
}

// `transform::desaturate_into_gamut`
fn desaturate_into_gamut(c: vec3<f32>, w: vec3<f32>) -> vec3<f32> {
    let mn = min(c.x, min(c.y, c.z));
    if (mn >= 0.0) {
        return c;
    }
    let y = dot(w, c);
    if (y <= 0.0) {
        return vec3(0.0);
    }
    let t = y / (y - mn);
    return max(vec3(y) + (c - vec3(y)) * t, vec3(0.0));
}

fn at(i: u32) -> f32 {
    return prog[min(i, arrayLength(&prog) - 1u)];
}

fn srgb_to_linear(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(v: f32) -> f32 {
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn pq_eotf(e: f32) -> f32 {
    let m1 = 0.15930176;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.851563;
    let c3 = 18.6875;
    let p = pow(max(e, 0.0), 1.0 / m2);
    return pow(max(p - c1, 0.0) / (c2 - c3 * p), 1.0 / m1);
}

fn pq_inverse_eotf(y: f32) -> f32 {
    let m1 = 0.15930176;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.851563;
    let c3 = 18.6875;
    let p = pow(max(y, 0.0), m1);
    return pow((c1 + c2 * p) / (1.0 + c3 * p), m2);
}

fn hlg_oetf(l: f32) -> f32 {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.5599107;
    if (l <= 1.0 / 12.0) {
        return sqrt(3.0 * max(l, 0.0));
    }
    return a * log(12.0 * l - b) + c;
}

fn hlg_inverse_oetf(e: f32) -> f32 {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.5599107;
    if (e <= 0.5) {
        return e * e / 3.0;
    }
    return (exp((e - c) / a) + b) / 12.0;
}

// DaVinci Intermediate (Blackmagic, DaVinci Resolve 17 Wide Gamut Intermediate)
const DI_A: f32 = 0.0075;
const DI_B: f32 = 7.0;
const DI_C: f32 = 0.07329248;
const DI_M: f32 = 10.44426855;
const DI_LIN_CUT: f32 = 0.00262409;
const DI_LOG_CUT: f32 = 0.02740668;

fn tf_decode(t: u32, v: f32) -> f32 {
    switch t {
        case 0u: { return srgb_to_linear(v); }
        case 1u: { return sign(v) * pow(abs(v), 2.4); }
        case 2u: { return pq_eotf(v); }
        case 3u: { return hlg_inverse_oetf(v); }
        default: {
            if (v <= DI_LOG_CUT) {
                return v / DI_M;
            }
            return exp2(v / DI_C - DI_B) - DI_A;
        }
    }
}

fn tf_encode(t: u32, x: f32) -> f32 {
    switch t {
        case 0u: { return linear_to_srgb(x); }
        case 1u: { return sign(x) * pow(abs(x), 1.0 / 2.4); }
        case 2u: { return pq_inverse_eotf(x); }
        case 3u: { return hlg_oetf(x); }
        default: {
            if (x <= DI_LIN_CUT) {
                return x * DI_M;
            }
            return (log2(x + DI_A) + DI_B) * DI_C;
        }
    }
}

// `Table::eval`: values at base.., n samples over lo..hi, linear in between and beyond the ends.
fn table(base: u32, n: u32, lo: f32, hi: f32, x: f32) -> f32 {
    let first = at(base);
    if (n < 2u) {
        return first;
    }
    let span = hi - lo;
    if (abs(span) <= 1.1920929e-7) {
        return first;
    }
    let t = (x - lo) / span * f32(n - 1u);
    let i = clamp(floor(t), 0.0, f32(n - 2u));
    let fr = t - i;
    let k = u32(i);
    let a = at(base + k);
    let b = at(base + k + 1u);
    return a + (b - a) * fr;
}

fn lggo_channel(v: f32, lift: f32, gamma: f32, gain: f32, off: f32) -> f32 {
    let a = lift + (gain - lift) * v;
    var p = a;
    if (gamma > 1e-6) {
        p = sign(a) * pow(abs(a), 1.0 / gamma);
    }
    return p + off;
}

// `ops::grain_hash` (lowbias32, wrapping).
fn grain_hash(x0: u32) -> u32 {
    var x = x0;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

// `ops::grain_cell`: a triangular value on −1…1.
fn grain_cell(ix: u32, iy: u32, seed: u32) -> f32 {
    let h = grain_hash(seed ^ grain_hash(iy ^ grain_hash(ix)));
    let a = f32(h >> 8u) * (1.0 / 16777216.0);
    let b = f32(grain_hash(h) >> 8u) * (1.0 / 16777216.0);
    return a + b - 1.0;
}

// `ops::grain_noise` (the encoder hands over finite values only).
fn grain_noise(xy: vec2<f32>, inv_size: f32, softness: f32, seed: u32) -> f32 {
    let inv = clamp(inv_size, 1.0 / 4096.0, 4.0);
    let soft = clamp(softness, 0.0, 1.0);
    let p = clamp((xy + vec2(0.5)) * inv, vec2(-1e7), vec2(1e7));
    let i = floor(p);
    let t = p - i;
    let s = soft * (t * t * (vec2(3.0) - 2.0 * t));
    let ix = bitcast<u32>(i32(i.x));
    let iy = bitcast<u32>(i32(i.y));
    let wx0 = 1.0 - s.x;
    let wx1 = s.x;
    let wy0 = 1.0 - s.y;
    let wy1 = s.y;
    let v = wy0 * (wx0 * grain_cell(ix, iy, seed) + wx1 * grain_cell(ix + 1u, iy, seed))
        + wy1 * (wx0 * grain_cell(ix, iy + 1u, seed) + wx1 * grain_cell(ix + 1u, iy + 1u, seed));
    let norm = max(sqrt((wx0 * wx0 + wx1 * wx1) * (wy0 * wy0 + wy1 * wy1)), 0.25);
    return v / norm;
}

// One op of a chain: `code` at `*pcp` on `c_in`; moves `*pcp` past the op (to `len` on an
// unknown code). A literal `code` (the specialised chains below) lets the compiler drop every
// other case.
// `Warp2D::lookup` of the (du, dv) field at `base` (`nc` × `nr`, row major); `space` 0 hue-sat
// (u wraps), 1 chroma-luma
fn warp_lookup(base: u32, nc: u32, nr: u32, space: u32, u0: f32, v0: f32) -> vec2<f32> {
    let fnc = f32(nc);
    let fnr = f32(nr);
    var i0: u32;
    var i1: u32;
    var fx: f32;
    if (space == 0u) {
        let x = (u0 - floor(u0)) * fnc;
        let i = clamp(floor(x), 0.0, fnc - 1.0);
        i0 = u32(i);
        i1 = select(i0 + 1u, 0u, i0 + 1u >= nc);
        fx = x - i;
    } else {
        let x = clamp(u0, 0.0, 1.0) * (fnc - 1.0);
        let i = clamp(floor(x), 0.0, fnc - 2.0);
        i0 = u32(i);
        i1 = i0 + 1u;
        fx = x - i;
    }
    let y = clamp(v0, 0.0, 1.0) * (fnr - 1.0);
    let j = clamp(floor(y), 0.0, fnr - 2.0);
    let fy = y - j;
    let j0 = u32(j);
    let r0 = base + j0 * nc * 2u;
    let r1 = r0 + nc * 2u;
    let a = vec2(at(r0 + i0 * 2u), at(r0 + i0 * 2u + 1u));
    let b = vec2(at(r0 + i1 * 2u), at(r0 + i1 * 2u + 1u));
    let cc = vec2(at(r1 + i0 * 2u), at(r1 + i0 * 2u + 1u));
    let d = vec2(at(r1 + i1 * 2u), at(r1 + i1 * 2u + 1u));
    let top = a + (b - a) * fx;
    let bot = cc + (d - cc) * fx;
    return top + (bot - top) * fy;
}

// `warp::hsv_to_rgb`
fn warp_hsv(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k0 = vec3(5.0, 3.0, 1.0) + vec3(h * 6.0);
    let k = k0 - 6.0 * floor(k0 / 6.0);
    return vec3(v) - vec3(v * s) * clamp(min(k, vec3(4.0) - k), vec3(0.0), vec3(1.0));
}

// `warp::scale_luma` (SCALE_FLOOR 1e-3, MAX_SCALE 16)
fn warp_luma(o: vec3<f32>, t: f32, w: vec3<f32>) -> vec3<f32> {
    let y2 = w.x * o.x + w.y * o.y + w.z * o.z;
    if (t >= 0.0 && y2 > 1e-3) {
        return o * clamp(t / y2, 0.0, 16.0);
    }
    return o + vec3(t - y2);
}

// `Warp2D::apply`
// whether every component is finite (by the exponent bits: comparisons with NaN may be folded away)
fn warp_finite(c: vec3<f32>) -> bool {
    let e = bitcast<vec3<u32>>(c) & vec3(0x7f800000u);
    return all(e != vec3(0x7f800000u));
}

fn warp2d(c: vec3<f32>, space: u32, keep: bool, w: vec3<f32>, nc: u32, nr: u32, base: u32) -> vec3<f32> {
    let mx = max(max(c.x, c.y), c.z);
    if (!warp_finite(c) || !(mx > 1e-6) || nc < 2u || nr < 2u) {
        return c;
    }
    let mn = min(min(c.x, c.y), c.z);
    let d = mx - mn;
    let s = d / mx;
    var h = 0.0;
    if (d >= 1e-7) {
        if (mx == c.x) {
            h = (c.y - c.z) / d + select(0.0, 6.0, c.y < c.z);
        } else if (mx == c.y) {
            h = (c.z - c.x) / d + 2.0;
        } else {
            h = (c.x - c.y) / d + 4.0;
        }
        h = h / 6.0;
    }
    let y = w.x * c.x + w.y * c.y + w.z * c.z;
    var uv = vec2(clamp(s, 0.0, 1.0), y);
    if (space == 0u) {
        uv = vec2(h, clamp(s, 0.0, 1.0));
    }
    let dd = warp_lookup(base, nc, nr, space, uv.x, uv.y);
    if (dd.x == 0.0 && dd.y == 0.0) {
        return c;
    }
    if (space == 0u) {
        let h2 = h + dd.x;
        let o = warp_hsv(h2 - floor(h2), max(s + dd.y, 0.0), mx);
        if (keep) {
            return warp_luma(o, y, w);
        }
        return o;
    }
    return warp_luma(warp_hsv(h, max(s + dd.x, 0.0), mx), max(y + dd.y, 0.0), w);
}

// 3D qualifier (fork G4.3, `filmcraft_color::key3d`): sub-op 0 the key (`hdr, soft, ni, ne`,
// then `x, y, z, r` per include sphere and per exclude sphere), 1 the despill (`dir, amount`)
const CO_KEY3D: u32 = 31u;

// `key3d::membership` of `p` in the `n` spheres at `base`
fn key3d_member(base: u32, n: u32, p: vec3<f32>, soft: f32) -> f32 {
    if (n == 0u) {
        return 0.0;
    }
    var d = 3.4e38;
    for (var i = 0u; i < n; i = i + 1u) {
        let b = base + i * 4u;
        let q = p - vec3(at(b), at(b + 1u), at(b + 2u));
        d = min(d, max(sqrt(q.x * q.x + q.y * q.y + q.z * q.z) - at(b + 3u), 0.0));
    }
    let t = clamp(d / soft, 0.0, 1.0);
    return 1.0 - t * t * (3.0 - 2.0 * t);
}

// `Key3d::matte` (spheres from `base`)
fn key3d_matte(c: vec3<f32>, hdr: bool, soft: f32, ni: u32, ne: u32, base: u32) -> f32 {
    if (!warp_finite(c)) {
        return 0.0;
    }
    let u0 = to_unit(c, hdr, unit_scale(c, hdr));
    // `key3d::opponent` (LUMA_WEIGHT 0.5)
    let p = vec3(0.5 * (u0.x + u0.y + u0.z) * 0.57735026, (u0.x - u0.y) * 0.70710677, (u0.x + u0.y - 2.0 * u0.z) * 0.4082483);
    let inc = key3d_member(base, ni, p, soft);
    if (inc <= 0.0) {
        return 0.0;
    }
    return inc * (1.0 - key3d_member(base + ni * 4u, ne, p, soft));
}

fn chain_op(code: u32, c_in: vec3<f32>, xy: vec2<f32>, pcp: ptr<function, u32>, len: u32) -> vec3<f32> {
    var c = c_in;
    var pc = *pcp;
    switch code {
        case CO_MATRIX: {
            let b = pc + 1u;
            let r = at(b) * c.x + at(b + 1u) * c.y + at(b + 2u) * c.z;
            let g = at(b + 3u) * c.x + at(b + 4u) * c.y + at(b + 5u) * c.z;
            let bl = at(b + 6u) * c.x + at(b + 7u) * c.y + at(b + 8u) * c.z;
            c = vec3(r + at(b + 9u), g + at(b + 10u), bl + at(b + 11u));
            pc = pc + 13u;
        }
        case CO_DECODE: {
            let t = u32(at(pc + 1u));
            c = vec3(tf_decode(t, c.x), tf_decode(t, c.y), tf_decode(t, c.z));
            pc = pc + 2u;
        }
        case CO_ENCODE: {
            let t = u32(at(pc + 1u));
            c = vec3(tf_encode(t, c.x), tf_encode(t, c.y), tf_encode(t, c.z));
            pc = pc + 2u;
        }
        case CO_TABLE: {
            let lo = at(pc + 1u);
            let hi = at(pc + 2u);
            let n = u32(at(pc + 3u));
            let base = pc + 4u;
            c = vec3(table(base, n, lo, hi, c.x), table(base, n, lo, hi, c.y), table(base, n, lo, hi, c.z));
            pc = base + n;
        }
        case CO_LGGO: {
            let b = pc + 1u;
            c = vec3(
                lggo_channel(c.x, at(b), at(b + 3u), at(b + 6u), at(b + 9u)),
                lggo_channel(c.y, at(b + 1u), at(b + 4u), at(b + 7u), at(b + 10u)),
                lggo_channel(c.z, at(b + 2u), at(b + 5u), at(b + 8u), at(b + 11u)),
            );
            pc = pc + 13u;
        }
        case CO_SATURATION: {
            let l = at(pc + 3u) * c.x + at(pc + 4u) * c.y + at(pc + 5u) * c.z;
            let spread = max(c.x, max(c.y, c.z)) - min(c.x, min(c.y, c.z));
            let s = at(pc + 1u) * (1.0 + at(pc + 2u) * (1.0 - clamp(spread, 0.0, 1.0)));
            c = vec3(l) + (c - vec3(l)) * s;
            pc = pc + 6u;
        }
        case CO_HLG_OOTF: {
            let ys = at(pc + 1u) * c.x + at(pc + 2u) * c.y + at(pc + 3u) * c.z;
            if (ys > 0.0) {
                c = c * (at(pc + 4u) * pow(ys, at(pc + 5u)));
            } else {
                c = vec3(0.0);
            }
            pc = pc + 6u;
        }
        case CO_TONE_MAP: {
            let src_peak = at(pc + 1u);
            let dst_peak = at(pc + 2u);
            let lo = at(pc + 3u);
            let hi = at(pc + 4u);
            let n = u32(at(pc + 5u));
            let base = pc + 6u;
            let m = max(c.x, max(c.y, c.z));
            if (m > 0.0) {
                let nits = m * 203.0;
                var k: f32;
                if (nits >= src_peak) {
                    k = dst_peak / 203.0 / m;
                } else {
                    k = table(base, n, lo, hi, pq_inverse_eotf(nits / 10000.0));
                }
                c = c * k;
            }
            pc = base + n;
        }
        case CO_GAMUT_COMPRESS: {
            c = gamut_compress(c);
            pc = pc + 1u;
        }
        case CO_DESATURATE: {
            c = desaturate_into_gamut(c, vec3(at(pc + 1u), at(pc + 2u), at(pc + 3u)));
            pc = pc + 4u;
        }
        case CO_CLAMP: {
            c = min(max(c, vec3(at(pc + 1u))), vec3(at(pc + 2u)));
            pc = pc + 3u;
        }
        case CO_TONE_REGIONS: {
            let l = at(pc + 3u) * c.x + at(pc + 4u) * c.y + at(pc + 5u) * c.z;
            let s = clamp(1.0 - l, 0.0, 1.0);
            let h = clamp(l, 0.0, 1.0);
            let nl = max(l + at(pc + 1u) * (s * s * s) + at(pc + 2u) * (h * h * h), 0.0);
            if (l > 1e-5) {
                c = c * (nl / l);
            }
            pc = pc + 6u;
        }
        case CO_CONTRAST: {
            let k = 1.0 + at(pc + 1u);
            let keep = at(pc + 2u) != 0.0;
            c = vec3(contrast(c.x, k, keep), contrast(c.y, k, keep), contrast(c.z, k, keep));
            pc = pc + 3u;
        }
        case CO_SPECULAR: {
            let g = at(pc + 1u);
            c = vec3(specular(c.x, g), specular(c.y, g), specular(c.z, g));
            pc = pc + 2u;
        }
        case CO_SPLIT_TONE: {
            let b = pc + 1u;
            let sh = vec3(at(b), at(b + 1u), at(b + 2u));
            let hi = vec3(at(b + 3u), at(b + 4u), at(b + 5u));
            let l = clamp(at(b + 6u) * c.x + at(b + 7u) * c.y + at(b + 8u) * c.z, 0.0, 1.0);
            c = c + (sh * (1.0 - l) + hi * l);
            pc = pc + 10u;
        }
        case CO_VIGNETTE: {
            let amount = at(pc + 1u);
            let nx = (xy.x / at(pc + 5u) - 0.5) * 2.0 * at(pc + 4u);
            let ny = (xy.y / at(pc + 6u) - 0.5) * 2.0;
            let d = sqrt(nx * nx + ny * ny) / 1.4142135;
            let edge = clamp((d - at(pc + 2u)) / max(at(pc + 3u), 1e-6), 0.0, 1.0);
            let e2 = edge * edge * (3.0 - 2.0 * edge);
            let k = 1.0 + amount * 0.2 * e2;
            if (amount < 0.0) {
                c = c * max(k, 0.0);
            } else {
                c = c + (vec3(1.0) - c) * (k - 1.0);
            }
            pc = pc + 7u;
        }
        case CO_CURVE: {
            let hdr = at(pc + 1u) != 0.0;
            let base = pc + 5u;
            var o = c;
            for (var k = 0u; k < 3u; k++) {
                if (at(pc + 2u + k) != 0.0) {
                    o[k] = curve_at(base, c[k], hdr);
                }
            }
            c = o;
            pc = table_end(base);
        }
        case CO_WHEELS: {
            let b = pc + 1u;
            let sh = vec3(at(b), at(b + 1u), at(b + 2u));
            let md = vec3(at(b + 3u), at(b + 4u), at(b + 5u));
            let hi = vec3(at(b + 6u), at(b + 7u), at(b + 8u));
            let l = clamp(at(b + 9u) * c.x + at(b + 10u) * c.y + at(b + 11u) * c.z, 0.0, 1.0);
            let wsh = (1.0 - l) * (1.0 - l);
            let whi = l * l;
            let wmid = max(1.0 - wsh - whi, 0.0);
            let v = c + sh * wsh;
            let v2 = v + md * wmid;
            c = v2 * (vec3(1.0) + hi * whi);
            pc = pc + 13u;
        }
        case CO_HSL_CURVES: {
            let hdr = at(pc + 1u) != 0.0;
            let t_hh = pc + 2u;
            let t_hs = table_end(t_hh);
            let t_hl = table_end(t_hs);
            let t_ls = table_end(t_hl);
            let t_ss = table_end(t_ls);
            let m = unit_scale(c, hdr);
            let u0 = to_unit(c, hdr, m);
            var h = rgb_to_hsl(u0);
            let h0 = h.x;
            let s0 = h.y;
            let l0 = h.z;
            if (at(t_hh + 2u) != 0.0) {
                h.x = fract_euclid(h.x + (curve_at(t_hh, h0, false) - 0.5));
            }
            var sm = 1.0;
            if (at(t_hs + 2u) != 0.0) {
                sm = sm * (curve_at(t_hs, h0, false) * 2.0);
            }
            if (at(t_ls + 2u) != 0.0) {
                sm = sm * (curve_at(t_ls, l0, false) * 2.0);
            }
            if (at(t_ss + 2u) != 0.0) {
                sm = sm * (curve_at(t_ss, s0, false) * 2.0);
            }
            h.y = clamp(h.y * sm, 0.0, 1.0);
            if (at(t_hl + 2u) != 0.0) {
                h.z = clamp(h.z + (curve_at(t_hl, h0, false) - 0.5) * 0.5, 0.0, 1.0);
            }
            var o = hsl_to_rgb(h.x, h.y, h.z);
            if (hdr) {
                o = o * m;
            }
            c = o;
            pc = table_end(t_ss);
        }
        case CO_LUT: {
            let k = at(pc + 1u);
            let hdr = at(pc + 2u) != 0.0;
            let sh = pc + 3u;
            let n1 = u32(at(sh));
            let cube = sh + 7u + n1 * 3u;
            let n3 = u32(at(cube));
            let m = unit_scale(c, hdr);
            var u = to_unit(c, hdr, m);
            if (n1 >= 2u) {
                u = lut1d(sh, u);
            }
            if (n3 >= 2u) {
                u = lut3d(cube, u);
            }
            if (hdr) {
                u = u * m;
            }
            c = c + (u - c) * k;
            pc = cube + 7u + n3 * n3 * n3 * 3u;
        }
        case CO_LOOK: {
            let n = u32(at(pc + 1u));
            let k = at(pc + 2u);
            let hdr = at(pc + 3u) != 0.0;
            var lk: vec3<f32>;
            if (hdr) {
                let m = unit_scale(c, true);
                lk = look(n, to_unit(c, true, m)) * m;
            } else {
                lk = look(n, c);
            }
            c = c + (lk - c) * k;
            pc = pc + 4u;
        }
        case CO_HSL_KEY: {
            let hdr = at(pc + 8u) != 0.0;
            let u0 = to_unit(c, hdr, unit_scale(c, hdr));
            let h = rgb_to_hsl(u0);
            let hue = at(pc + 1u);
            let soft = at(pc + 7u);
            let dh = min(abs(h.x - hue), 1.0 - abs(h.x - hue));
            let mh = 1.0 - clamp((dh - at(pc + 2u) / 2.0) / soft, 0.0, 1.0);
            var ms = clamp((h.y - at(pc + 3u)) / soft, 0.0, 1.0);
            let sat_max = at(pc + 4u);
            if (sat_max < 1.0) {
                ms = min(ms, clamp((sat_max - h.y) / soft, 0.0, 1.0));
            }
            let ml = min(clamp((h.z - at(pc + 5u)) / soft, 0.0, 1.0), clamp((at(pc + 6u) - h.z) / soft, 0.0, 1.0));
            mask_reg = mh * ms * ml;
            pc = pc + 9u;
        }
        case CO_KEY3D: {
            if (at(pc + 1u) == 0.0) {
                let ni = u32(at(pc + 4u));
                let ne = u32(at(pc + 5u));
                mask_reg = key3d_matte(c, at(pc + 2u) != 0.0, at(pc + 3u), ni, ne, pc + 6u);
                pc = pc + 6u + (ni + ne) * 4u;
            } else {
                let dir = vec3(at(pc + 2u), at(pc + 3u), at(pc + 4u));
                let k = c.x * dir.x + c.y * dir.y + c.z * dir.z;
                // `key3d::despill`: non-finite pixels pass unchanged
                if (warp_finite(c) && warp_finite(vec3(k)) && k > 0.0) {
                    let s = at(pc + 5u) * k;
                    c = c - s * dir;
                }
                pc = pc + 6u;
            }
        }
        case CO_STASH: {
            stash_reg = c;
            pc = pc + 1u;
        }
        case CO_KEY_BEGIN: {
            stash_reg = c;
            mask_reg = 1.0;
            pc = pc + 1u;
        }
        case CO_KEY_END: {
            c = stash_reg;
            pc = pc + 1u;
        }
        case CO_MIX_STASH: {
            var m = mask_reg;
            if (at(pc + 1u) != 0.0) {
                m = 1.0 - m;
            }
            mask_reg = m;
            c = stash_reg + (c - stash_reg) * m;
            pc = pc + 2u;
        }
        case CO_WARP: {
            let nc = u32(at(pc + 6u));
            let nr = u32(at(pc + 7u));
            let w = vec3(at(pc + 3u), at(pc + 4u), at(pc + 5u));
            c = warp2d(c, u32(at(pc + 1u)), at(pc + 2u) != 0.0, w, nc, nr, pc + 8u);
            pc = pc + 8u + nc * nr * 2u;
        }
        case CO_HSL_CORRECT: {
            let shift = at(pc + 1u);
            let sat = at(pc + 2u);
            let temp = at(pc + 3u);
            let tint = at(pc + 4u);
            let show = u32(at(pc + 5u));
            let hdr = at(pc + 6u) != 0.0;
            let m = mask_reg;
            let scale = unit_scale(c, hdr);
            let u0 = to_unit(c, hdr, scale);
            var o: vec3<f32>;
            switch show {
                case 1u: {
                    let g = rgb_to_hsl(u0).z;
                    o = vec3(g) + (u0 - vec3(g)) * m;
                }
                case 2u: {
                    o = u0 * m;
                }
                case 3u: {
                    o = vec3(m);
                }
                default: {
                    var hh = rgb_to_hsl(u0);
                    hh.x = fract_euclid(hh.x + shift);
                    hh.y = clamp(hh.y * sat, 0.0, 1.0);
                    var c2 = hsl_to_rgb(hh.x, hh.y, hh.z);
                    c2.x = c2.x * (1.0 + 0.25 * temp);
                    c2.z = c2.z * (1.0 - 0.25 * temp);
                    c2.y = c2.y * (1.0 - 0.2 * tint);
                    o = (u0 + (c2 - u0) * m) * scale;
                }
            }
            c = o;
            pc = pc + 7u;
        }
        case CO_GRAIN: {
            let seed = u32(at(pc + 4u)) | (u32(at(pc + 5u)) << 16u);
            let l = clamp(at(pc + 6u) * c.x + at(pc + 7u) * c.y + at(pc + 8u) * c.z, 0.0, 1.0);
            // GRAIN_FLOOR 0.1, GRAIN_AMPLITUDE 0.15 (`filmcraft_color::ops`)
            let wl = 0.1 + 0.9 * 4.0 * l * (1.0 - l);
            let d = at(pc + 1u) * 0.15 * wl * grain_noise(xy, at(pc + 2u), at(pc + 3u), seed);
            c = c + vec3(d);
            pc = pc + 9u;
        }
        case CO_ZONE_WHEELS: {
            // `ops::ZoneWheels::apply`: weights, pivot, global (exposure, sat, shift), then six
            // zones (edge, falloff, exposure, sat, shift), the first three dark
            let w = vec3(at(pc + 1u), at(pc + 2u), at(pc + 3u));
            let y = w.x * c.x + w.y * c.y + w.z * c.z;
            let r = y / at(pc + 4u);
            var s = -30.0;
            if (r > exp2(-30.0)) {
                s = min(log2(r), 30.0);
            }
            var e = at(pc + 5u);
            var sat = at(pc + 6u);
            var d = vec3(at(pc + 7u), at(pc + 8u), at(pc + 9u));
            for (var i = 0u; i < 6u; i = i + 1u) {
                let b = pc + 10u + i * 7u;
                let k = zone_weight(i < 3u, s, at(b), at(b + 1u));
                e = e + k * at(b + 2u);
                sat = sat * (1.0 + k * (at(b + 3u) - 1.0));
                d = d + k * vec3(at(b + 4u), at(b + 5u), at(b + 6u));
            }
            // ZONE_MAX_STOPS 16, ZONE_MAX_SAT 65536, ZONE_MAX_VALUE 1e30
            let gain = exp2(min(max(e, -16.0), 16.0));
            let sat2 = min(max(sat, 0.0), 65536.0);
            let c2 = (c + vec3(y) * d) * gain;
            let y2 = w.x * c2.x + w.y * c2.y + w.z * c2.z;
            let o = vec3(y2) + (c2 - vec3(y2)) * sat2;
            // a NaN result is 0, as on the CPU
            c = select(vec3(0.0), min(max(o, vec3(-1e30)), vec3(1e30)), o == o);
            pc = pc + 52u;
        }
        default: {
            // unknown code: stop (the encoder never writes one)
            pc = len;
        }
    }
    *pcp = pc;
    return c;
}

// The chain interpreter: the program's codes read per pixel. `colorops.rs` `specialise` replaces
// this function (between the markers) with one call per op whose code is a literal (fork
// U0.2.3b: the interpreter's switch costs Intel's iGPU ~50 ms per op at 6.2K).
// RUN_CHAIN_BEGIN
fn run_chain(c0: vec3<f32>, xy: vec2<f32>) -> vec3<f32> {
    var c = c0;
    stash_reg = c0;
    var pc: u32 = 0u;
    let len = min(u.i1.x, arrayLength(&prog));
    loop {
        if (pc >= len) {
            break;
        }
        c = chain_op(u32(at(pc)), c, xy, &pc, len);
    }
    return c;
}
// RUN_CHAIN_END

@compute @workgroup_size(16, 16)
fn chain_px(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u.w || id.y >= u.h) {
        return;
    }
    let p = textureLoad(src, vec2<i32>(id.xy), 0);
    if (p.a <= 1e-6) {
        textureStore(dst, vec2<i32>(id.xy), p);
        return;
    }
    let c = run_chain(p.rgb / p.a, vec2<f32>(f32(id.x), f32(id.y)));
    textureStore(dst, vec2<i32>(id.xy), vec4(c * p.a, p.a));
}

// A key chain: the mask register it leaves, in every channel (0 where transparent).
@compute @workgroup_size(16, 16)
fn chain_key(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u.w || id.y >= u.h) {
        return;
    }
    let p = textureLoad(src, vec2<i32>(id.xy), 0);
    if (p.a <= 1e-6) {
        textureStore(dst, vec2<i32>(id.xy), vec4(0.0));
        return;
    }
    mask_reg = 1.0;
    let c = run_chain(p.rgb / p.a, vec2<f32>(f32(id.x), f32(id.y)));
    textureStore(dst, vec2<i32>(id.xy), vec4(mask_reg + 0.0 * c.x));
}

// A masked chain: corrects the image in `aux` with the mask in `src` (red).
@compute @workgroup_size(16, 16)
fn chain_masked(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u.w || id.y >= u.h) {
        return;
    }
    let p = textureLoad(aux, vec2<i32>(id.xy), 0);
    if (p.a <= 1e-6) {
        textureStore(dst, vec2<i32>(id.xy), p);
        return;
    }
    mask_reg = textureLoad(src, vec2<i32>(id.xy), 0).x;
    let c = run_chain(p.rgb / p.a, vec2<f32>(f32(id.x), f32(id.y)));
    textureStore(dst, vec2<i32>(id.xy), vec4(c * p.a, p.a));
}

// ---- mask coverage (`filmcraft_render::mask`, the maths of `mask.wgsl`) on a program of
// `count, (mode, inverted, feather, expansion, opacity, n, x, y…)…`

fn mask_falloff(s: f32, feather: f32) -> f32 {
    let w = max(feather, 1.0);
    let t = clamp(s / w + 0.5, 0.0, 1.0);
    let smooth_t = t * t * (3.0 - 2.0 * t);
    return t + (smooth_t - t) * min(feather, 1.0);
}

fn mask_mode_start(mode: u32) -> f32 {
    if (mode == 2u || mode == 3u || mode == 5u) {
        return 1.0;
    }
    return 0.0;
}

fn mask_combine(mode: u32, a: f32, m: f32) -> f32 {
    switch mode {
        case 1u: { return a + m - a * m; }
        case 2u: { return a * (1.0 - m); }
        case 3u: { return a * m; }
        case 4u: { return max(a, m); }
        case 5u: { return min(a, m); }
        case 6u: { return a + m - 2.0 * a * m; }
        default: { return a; }
    }
}

fn mask_seg_dist2(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let d = b - a;
    let q = p - a;
    let l2 = dot(d, d);
    var t = 0.0;
    if (l2 > 0.0) {
        t = clamp(dot(q, d) / l2, 0.0, 1.0);
    }
    let e = q - d * t;
    return dot(e, e);
}

fn mask_coverage(p: vec2<f32>) -> f32 {
    let count = u32(at(0u));
    var acc = mask_mode_start(u32(at(1u)));
    var b = 1u;
    for (var i = 0u; i < count; i++) {
        let mode = u32(at(b));
        let inverted = at(b + 1u) != 0.0;
        let feather = at(b + 2u);
        let expansion = at(b + 3u);
        let opacity = at(b + 4u);
        let n = u32(at(b + 5u));
        let pts = b + 6u;
        let band = abs(expansion) + max(feather, 1.0) * 0.5 + 1.0;
        var sd = -band;
        if (n >= 3u) {
            var wind = 0;
            var d2 = band * band;
            for (var k = 0u; k < n; k++) {
                let a = vec2(at(pts + 2u * k), at(pts + 2u * k + 1u));
                let k1 = (k + 1u) % n;
                let c = vec2(at(pts + 2u * k1), at(pts + 2u * k1 + 1u));
                if ((a.y <= p.y) != (c.y <= p.y)) {
                    let t = (p.y - a.y) / (c.y - a.y);
                    if (a.x + (c.x - a.x) * t < p.x) {
                        if (c.y > a.y) { wind = wind + 1; } else { wind = wind - 1; }
                    }
                }
                d2 = min(d2, mask_seg_dist2(p, a, c));
            }
            let d = sqrt(d2);
            if (wind != 0) { sd = d; } else { sd = -d; }
        }
        var v = mask_falloff(sd + expansion, feather);
        if (inverted) {
            v = 1.0 - v;
        }
        acc = clamp(mask_combine(mode, acc, opacity * v), 0.0, 1.0);
        b = pts + 2u * n;
    }
    return acc;
}

// A masked effect's mix with the image before it (code 3: `aux`), or opacity masks (4).
@compute @workgroup_size(16, 16)
fn mask_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= u.w || id.y >= u.h) {
        return;
    }
    let cov = mask_coverage(vec2<f32>(f32(id.x) + 0.5, f32(id.y) + 0.5));
    let e = textureLoad(src, vec2<i32>(id.xy), 0);
    if (u.code == 3u) {
        let o = textureLoad(aux, vec2<i32>(id.xy), 0);
        textureStore(dst, vec2<i32>(id.xy), o + (e - o) * cov);
    } else {
        textureStore(dst, vec2<i32>(id.xy), e * cov);
    }
}

