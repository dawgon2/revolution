// FilmCraft GPU effect stage: the standard effects of `filmcraft_render::gpufx::FxOp`, one
// compute pass each, reading the working image (`src`, linear premultiplied RGBA f32) and writing
// the next one (`dst`). The math mirrors `FxOp::apply` (the CPU reference) operation by
// operation; parameters arrive evaluated (keyframes, defaults and clamps applied on the CPU).

struct U {
    i0: vec4<u32>,  // op, width, height, unused
    i1: vec4<u32>,  // integer parameters (box radius, repeat, steps, channel, rect…)
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    p4: vec4<f32>,
    p5: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba32float, write>;
// Unsharp Mask: the image before blurring.
@group(0) @binding(3) var aux: texture_2d<f32>;

const OP_BRIGHTNESS_CONTRAST: u32 = 1u;
const OP_PROC_AMP: u32 = 2u;
const OP_TINT: u32 = 3u;
const OP_BLACK_WHITE: u32 = 4u;
const OP_COLOR_BALANCE: u32 = 5u;
const OP_LEAVE_COLOR: u32 = 6u;
const OP_CHANGE_TO_COLOR: u32 = 7u;
const OP_COLOR_PASS: u32 = 8u;
const OP_GAMMA: u32 = 9u;
const OP_LEVELS: u32 = 10u;
const OP_EXTRACT: u32 = 11u;
const OP_INVERT: u32 = 12u;
const OP_INVERT_ALPHA: u32 = 13u;
const OP_POSTERIZE: u32 = 14u;
const OP_ASC_CDL: u32 = 15u;
const OP_CHANNEL_MIX: u32 = 16u;
const OP_COLOR_REPLACE: u32 = 17u;
const OP_ALPHA_ADJUST: u32 = 18u;
const OP_VIGNETTE: u32 = 19u;
const OP_BOX: u32 = 20u;
const OP_DIRECTIONAL: u32 = 22u;
const OP_UNSHARP: u32 = 23u;
const OP_CROP: u32 = 24u;
const OP_RESAMPLE: u32 = 25u;
const OP_HFLIP: u32 = 26u;
const OP_VFLIP: u32 = 27u;
const OP_MIRROR: u32 = 28u;
const OP_OFFSET: u32 = 29u;
const OP_MEDIAN_MASK: u32 = 30u;
const OP_VIDEO_LIMITER: u32 = 31u;
const OP_DENOISE: u32 = 32u;
const OP_DENOISE_YCC: u32 = 33u;
const OP_TEMPORAL_INIT: u32 = 34u;
const OP_UNSHARP_DIRECT: u32 = 36u;
const OP_LUMA_KEY: u32 = 37u;
const OP_UKEY_MATTE: u32 = 38u;
const OP_UKEY_CHOKE: u32 = 39u;
const OP_UKEY: u32 = 40u;
const OP_COLOR_KEY: u32 = 41u;

fn size() -> vec2<i32> {
    return vec2<i32>(i32(u.i0.y), i32(u.i0.z));
}

fn ld(p: vec2<i32>) -> vec4<f32> {
    return textureLoad(src, p, 0);
}

// `Image::get_or_clear`
fn ld_or_clear(p: vec2<i32>) -> vec4<f32> {
    let s = size();
    if p.x < 0 || p.y < 0 || p.x >= s.x || p.y >= s.y {
        return vec4(0.0);
    }
    return textureLoad(src, p, 0);
}

// `Image::sample_bilinear`: pixel centres at +0.5, transparent outside.
fn sample_bilinear(x: f32, y: f32) -> vec4<f32> {
    let fx = x - 0.5;
    let fy = y - 0.5;
    let x0 = floor(fx);
    let y0 = floor(fy);
    let tx = fx - x0;
    let ty = fy - y0;
    let i = vec2<i32>(i32(x0), i32(y0));
    let a = ld_or_clear(i);
    let b = ld_or_clear(i + vec2(1, 0));
    let c = ld_or_clear(i + vec2(0, 1));
    let d = ld_or_clear(i + vec2(1, 1));
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    return top + (bot - top) * ty;
}

// `Image::sample_bilinear_clamped`
fn sample_clamped(x: f32, y: f32) -> vec4<f32> {
    let s = size();
    let fx = clamp(x - 0.5, 0.0, f32(s.x) - 1.0);
    let fy = clamp(y - 0.5, 0.0, f32(s.y) - 1.0);
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let x1 = min(x0 + 1, s.x - 1);
    let y1 = min(y0 + 1, s.y - 1);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let a = ld(vec2(x0, y0));
    let b = ld(vec2(x1, y0));
    let c = ld(vec2(x0, y1));
    let d = ld(vec2(x1, y1));
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    return top + (bot - top) * ty;
}

// ---- colour helpers (`filmcraft_color`, `effects::enc` / `dec`)

fn linear_to_srgb1(v: f32) -> f32 {
    if v <= 0.0031308 {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn srgb_to_linear1(v: f32) -> f32 {
    if v <= 0.04045 {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn enc(c: vec3<f32>) -> vec3<f32> {
    let m = max(c, vec3(0.0));
    return vec3(linear_to_srgb1(m.x), linear_to_srgb1(m.y), linear_to_srgb1(m.z));
}

fn dec(c: vec3<f32>) -> vec3<f32> {
    let m = clamp(c, vec3(0.0), vec3(1.0));
    return vec3(srgb_to_linear1(m.x), srgb_to_linear1(m.y), srgb_to_linear1(m.z));
}

fn luma709(c: vec3<f32>) -> f32 {
    return 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
}

// `f32::powf` for x >= 0 (WGSL leaves pow(0, y) undefined).
fn powf(x: f32, y: f32) -> f32 {
    if x == 0.0 {
        if y == 0.0 {
            return 1.0;
        }
        if y > 0.0 {
            return 0.0;
        }
        return 3.0e38;
    }
    return pow(x, y);
}

// `f32::round`: halves away from zero (WGSL `round` rounds them to even).
fn round_away(x: f32) -> f32 {
    let a = abs(x);
    let f = floor(a);
    let r = select(f, f + 1.0, a - f >= 0.5);
    return select(r, -r, x < 0.0);
}

// `rem_euclid(1.0)`
fn fract_euclid(x: f32) -> f32 {
    return x - floor(x);
}

// `vfx::smoothstep` (not WGSL's: a degenerate edge pair divides by 1e-6)
fn smoothstep_fx(e0: f32, e1: f32, x: f32) -> f32 {
    let t = clamp((x - e0) / max(e1 - e0, 1e-6), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
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

fn linear_to_enc_limiter(v: f32) -> f32 {
    if v <= 1.0 {
        return linear_to_srgb1(max(v, 0.0));
    }
    return 1.0 + (v - 1.0) / 2.4;
}

fn enc_to_linear_limiter(v: f32) -> f32 {
    if v <= 1.0 {
        return srgb_to_linear1(max(v, 0.0));
    }
    return 1.0 + (v - 1.0) * 2.4;
}

fn knee_limiter(v: f32, max_val: f32, comp: f32) -> f32 {
    let k = max_val * (1.0 - comp);
    if comp <= 0.0 || v <= k {
        return min(v, max_val);
    }
    let r = max_val - k;
    return k + r * (1.0 - exp(-(v - k) / r));
}

fn to_rgb_709(y: f32, cb: f32, cr: f32) -> vec3<f32> {
    let kr = 0.2126;
    let kb = 0.0722;
    let kg = 1.0 - kr - kb;
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / kg;
    return vec3(r, g, b);
}

fn limit_video(v: vec3<f32>, max_val: f32, comp: f32, axis: u32) -> vec3<f32> {
    let kr = 0.2126;
    let kb = 0.0722;
    let kg = 1.0 - kr - kb;
    var y = kr * v.x + kg * v.y + kb * v.z;
    var cb = (v.z - y) / (2.0 * (1.0 - kb));
    var cr = (v.x - y) / (2.0 * (1.0 - kr));

    if axis != 1u {
        y = knee_limiter(max(y, 0.0), max_val, comp);
    }

    if axis != 0u {
        let rgb = to_rgb_709(y, cb, cr);
        var s = 1.0;
        let d = rgb - vec3(y);

        if rgb.x > max_val && d.x > 1e-6 {
            s = min(s, max((max_val - y) / d.x, 0.0));
        }
        if rgb.x < 0.0 && d.x < -1e-6 {
            s = min(s, max(y / (-d.x), 0.0));
        }

        if rgb.y > max_val && d.y > 1e-6 {
            s = min(s, max((max_val - y) / d.y, 0.0));
        }
        if rgb.y < 0.0 && d.y < -1e-6 {
            s = min(s, max(y / (-d.y), 0.0));
        }

        if rgb.z > max_val && d.z > 1e-6 {
            s = min(s, max((max_val - y) / d.z, 0.0));
        }
        if rgb.z < 0.0 && d.z < -1e-6 {
            s = min(s, max(y / (-d.z), 0.0));
        }

        if axis == 3u && s < 1.0 - 1e-4 {
            s = min(knee_limiter(s, 1.0, max(comp, 0.03)), 1.0);
        }
        cb *= s;
        cr *= s;
    }

    let out_rgb = to_rgb_709(y, cb, cr);
    return clamp(out_rgb, vec3(0.0), vec3(max_val));
}

// ---- per-pixel colour effects on straight colour (`Image::map_rgb`)

fn color_op(op: u32, c: vec3<f32>) -> vec3<f32> {
    switch op {
        case OP_BRIGHTNESS_CONTRAST: {
            let br = u.p0.x; let co = u.p0.y;
            return dec((enc(c) - 0.5) * co + 0.5 + br);
        }
        case OP_PROC_AMP: {
            let br = u.p0.x; let co = u.p0.y; let hue = u.p0.z; let sat = u.p0.w;
            var hsl = rgb_to_hsl(enc(c));
            hsl.x = fract_euclid(hsl.x + hue);
            hsl.y = clamp(hsl.y * sat, 0.0, 1.0);
            return dec((hsl_to_rgb(hsl.x, hsl.y, hsl.z) - 0.5) * co + 0.5 + br);
        }
        case OP_TINT: {
            let bl = u.p0.xyz; let wh = u.p1.xyz; let amt = u.p0.w;
            let l = linear_to_srgb1(max(luma709(c), 0.0));
            let t = bl + (wh - bl) * l;
            let e = enc(c);
            return dec(e + (t - e) * amt);
        }
        case OP_BLACK_WHITE: {
            return vec3(luma709(c));
        }
        case OP_COLOR_BALANCE: {
            let e = enc(c);
            let l = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
            let ws = (1.0 - l) * (1.0 - l);
            let wh = l * l;
            let wm = 1.0 - ws - wh;
            var o = e + u.p0.xyz * ws + u.p1.xyz * max(wm, 0.0) + u.p2.xyz * wh;
            if u.i1.x != 0u {
                let l2 = 0.2126 * o.x + 0.7152 * o.y + 0.0722 * o.z;
                o = o + (l - l2);
            }
            return dec(o);
        }
        case OP_LEAVE_COLOR: {
            let amt = u.p0.x; let kh = u.p0.y; let tol = u.p0.z; let soft = u.p0.w;
            let h = rgb_to_hsl(enc(c)).x;
            let d = min(abs(h - kh), 1.0 - abs(h - kh)) * 2.0;
            let keep = 1.0 - clamp((d - tol) / soft, 0.0, 1.0);
            let l = luma709(c);
            let k = amt * (1.0 - keep);
            return c + (l - c) * k;
        }
        case OP_CHANGE_TO_COLOR: {
            let fh = u.p0.x; let th = u.p0.y; let tol = u.p0.z; let soft = u.p0.w;
            var hsl = rgb_to_hsl(enc(c));
            let d = min(abs(hsl.x - fh), 1.0 - abs(hsl.x - fh));
            let w = 1.0 - clamp((d - tol) / soft, 0.0, 1.0);
            hsl.x = fract_euclid(hsl.x + (th - fh) * w);
            return dec(hsl_to_rgb(hsl.x, hsl.y, hsl.z));
        }
        case OP_COLOR_PASS: {
            let e = enc(c);
            let k = e - u.p0.xyz;
            let d = sqrt(k.x * k.x + k.y * k.y + k.z * k.z);
            let passes = (d <= u.p0.w * 1.2) != (u.i1.x != 0u);
            if passes {
                return c;
            }
            return vec3(luma709(c));
        }
        case OP_GAMMA: {
            let e = enc(c);
            let g = u.p0.x;
            return dec(vec3(powf(max(e.x, 0.0), g), powf(max(e.y, 0.0), g), powf(max(e.z, 0.0), g)));
        }
        case OP_LEVELS: {
            let ib = u.p0.x; let iw = u.p0.y; let ob = u.p0.z; let ow = u.p0.w; let g = u.p1.x;
            let v = clamp((enc(c) - ib) / (iw - ib), vec3(0.0), vec3(1.0));
            return dec(ob + vec3(powf(v.x, g), powf(v.y, g), powf(v.z, g)) * (ow - ob));
        }
        case OP_EXTRACT: {
            let lo = u.p0.x; let hi = u.p0.y; let soft = u.p0.z;
            let l = linear_to_srgb1(max(luma709(c), 0.0));
            let inside = min(clamp((l - lo) / soft, 0.0, 1.0), clamp((hi - l) / soft, 0.0, 1.0));
            return vec3(select(inside, 1.0 - inside, u.i1.x != 0u));
        }
        case OP_INVERT: {
            let ch = u.i1.x;
            let blend = u.p0.x;
            let e = enc(c);
            var o = e;
            if ch == 0u || ch == 1u { o.x = 1.0 - e.x; }
            if ch == 0u || ch == 2u { o.y = 1.0 - e.y; }
            if ch == 0u || ch == 3u { o.z = 1.0 - e.z; }
            return dec(o + (e - o) * blend);
        }
        case OP_POSTERIZE: {
            let n = u.p0.x;
            let e = enc(c) * n;
            return dec(vec3(round_away(e.x), round_away(e.y), round_away(e.z)) / n);
        }
        case OP_ASC_CDL: {
            let e = enc(c);
            let s = u.p0.xyz; let o = u.p1.xyz; let pw = max(u.p2.xyz, vec3(0.0)); let sat = u.p0.w;
            let q = clamp(e * s + o, vec3(0.0), vec3(1.0));
            let v = vec3(powf(q.x, pw.x), powf(q.y, pw.y), powf(q.z, pw.z));
            let l = luma709(v);
            return dec(clamp(l + sat * (v - l), vec3(0.0), vec3(1.0)));
        }
        case OP_CHANNEL_MIX: {
            let v = enc(c);
            let r = u.p0; let g = u.p1; let b = u.p2;
            return dec(clamp(vec3(r.x * v.x + r.y * v.y + r.z * v.z + r.w, g.x * v.x + g.y * v.y + g.z * v.z + g.w, b.x * v.x + b.y * v.y + b.z * v.z + b.w), vec3(0.0), vec3(1.0)));
        }
        case OP_COLOR_REPLACE: {
            let v = enc(c);
            let t = u.p0.xyz; let sim = u.p0.w;
            let dd = v - t;
            let d = sqrt(dd.x * dd.x + dd.y * dd.y + dd.z * dd.z);
            let k = 1.0 - smoothstep_fx(sim * 0.85, max(sim, 1e-4), d);
            if k <= 0.0 {
                return c;
            }
            var repl = u.p1.xyz;
            if u.i1.x == 0u {
                let rh = u.p2.xyz;
                repl = hsl_to_rgb(rh.x, rh.y, rgb_to_hsl(v).z);
            }
            return dec(v + (repl - v) * k);
        }
        default: {
            return c;
        }
    }
}

// ---- noise reduction (`filmcraft_render::denoise`)

// `denoise::encode`: sRGB on 0…1, its linear toe below, a linear extension above 1.
fn nr_enc1(v: f32) -> f32 {
    if v <= 0.0031308 {
        return v * 12.92;
    }
    if v <= 1.0 {
        return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
    }
    return 1.0 + (v - 1.0) / 2.4;
}

fn nr_dec1(e: f32) -> f32 {
    if e <= 0.04045 {
        return e / 12.92;
    }
    if e <= 1.0 {
        return pow((e + 0.055) / 1.055, 2.4);
    }
    return 1.0 + (e - 1.0) * 2.4;
}

// `denoise::to_ycc`: (Y, Cb = B′ − Y, Cr = R′ − Y) of the encoded straight colour.
fn nr_ycc(c: vec3<f32>) -> vec3<f32> {
    let e = vec3(nr_enc1(c.x), nr_enc1(c.y), nr_enc1(c.z));
    let y = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
    return vec3(y, e.z - y, e.x - y);
}

// `denoise::from_ycc`
fn nr_rgb(v: vec3<f32>) -> vec3<f32> {
    let r = v.z + v.x;
    let b = v.y + v.x;
    let g = (v.x - 0.2126 * r - 0.0722 * b) / 0.7152;
    return vec3(nr_dec1(r), nr_dec1(g), nr_dec1(b));
}

// Whether `v` is finite (exponent bits; `v != v` may be optimised away).
fn finite1(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}

fn finite4(v: vec4<f32>) -> bool {
    return finite1(v.x) && finite1(v.y) && finite1(v.z) && finite1(v.w);
}

// `denoise::pixel_ycc`: (Y, Cb, Cr, α), zeros for a skipped pixel (transparent or non-finite).
fn denoise_ycc(p: vec2<i32>) -> vec4<f32> {
    let o = ld(p);
    if !finite1(o.a) || o.a <= 1e-6 {
        return vec4(0.0);
    }
    let c = vec4(nr_ycc(o.rgb / o.a), o.a);
    if !finite4(c) {
        return vec4(0.0);
    }
    return c;
}

// `g(|d|)`, the spatial weight table (p1, p2).
fn nr_g(d: i32) -> f32 {
    let a = abs(d);
    if a < 4 {
        return u.p1[a];
    }
    return u.p2[min(a - 4, 3)];
}

// `denoise::apply` for one pixel: src = (Y, Cb, Cr, α) from `denoise_ycc`, aux = the original;
// i1 = (radius, luma on, chroma on), p0 = (1/2σl², 1/2σc²).
fn denoise_px(p: vec2<i32>) -> vec4<f32> {
    let s = size();
    let c0 = ld(p);
    let orig = textureLoad(aux, p, 0);
    if c0.w <= 0.0 {
        return orig;
    }
    let r = i32(min(u.i1.x, 7u));
    let luma_on = u.i1.y != 0u;
    let chroma_on = u.i1.z != 0u;
    let kl = u.p0.x;
    let kc = u.p0.y;
    var sl = 0.0;
    var wl = 0.0;
    var sc = vec2(0.0);
    var wc = 0.0;
    for (var y = max(p.y - r, 0); y <= min(p.y + r, s.y - 1); y++) {
        let gy = nr_g(y - p.y);
        for (var x = max(p.x - r, 0); x <= min(p.x + r, s.x - 1); x++) {
            let c = ld(vec2(x, y));
            if c.w <= 0.0 {
                continue;
            }
            let ws = nr_g(x - p.x) * gy * c.w;
            if luma_on {
                let dl = c.x - c0.x;
                let w1 = ws * exp(-(dl * dl) * kl);
                sl += w1 * c.x;
                wl += w1;
            }
            if chroma_on {
                let d1 = c.y - c0.y;
                let d2 = c.z - c0.z;
                let w2 = ws * exp(-(d1 * d1 + d2 * d2) * kc);
                sc += w2 * c.yz;
                wc += w2;
            }
        }
    }
    var yy = c0.x;
    if luma_on && wl > 0.0 {
        yy = sl / wl;
    }
    var cc = c0.yz;
    if chroma_on && wc > 0.0 {
        cc = sc / wc;
    }
    let out = vec4(nr_rgb(vec3(yy, cc)) * c0.w, c0.w);
    if !finite4(out) {
        return orig;
    }
    // (alpha: the original's, bit for bit)
    return vec4(out.rgb, orig.a);
}

// One output pixel of every op but the running-sum box blur.
// Unsharp Mask's mix of the original `o` with its blur `bq` (amount p0.x, threshold p0.y).
fn unsharp_mix(o_in: vec4<f32>, bq: vec4<f32>) -> vec4<f32> {
    var o = o_in;
    let amount = u.p0.x;
    let th = u.p0.y;
    for (var k = 0; k < 3; k++) {
        let d = o[k] - bq[k];
        if abs(d) >= th * o.a {
            o[k] = max(o[k] + d * amount, 0.0);
        }
    }
    return o;
}

// ---- Ultra Key (fork Y3.1b, `FxOp::UltraKey`; CPU reference `vfx::color::ultra_key_apply`).
// p0 = key Y′ Cb Cr, |key chroma|²; p1 = tolerance, gain, highlight, shadow; p2 = pedestal,
// choke, contrast, mid point; p3 = spill, desaturate, range, spill luma; p4.xyz = colour
// correction saturation, hue (turns), luminance. i1 = dominant channel, output, colour
// correction on, matte in src (the combine reads the cleaned-up matte instead of keying again).

fn ukey_ycbcr(v: vec3<f32>) -> vec3<f32> {
    let kr = 0.2126;
    let kb = 0.0722;
    let kg = 1.0 - kr - kb;
    let y = kr * v.x + kg * v.y + kb * v.z;
    return vec3(y, (v.z - y) / (2.0 * (1.0 - kb)), (v.x - y) / (2.0 * (1.0 - kr)));
}

// `vfx::color::ultra_key_matte`: the matte of a premultiplied pixel before cleanup.
fn ukey_matte(o: vec4<f32>) -> f32 {
    if o.a <= 1e-6 {
        return 0.0;
    }
    let k = u.p0.xyz;
    let kmag2 = u.p0.w;
    let v = enc(o.rgb / o.a);
    let y = ukey_ycbcr(v);
    let proj = (y.y * k.y + y.z * k.z) / kmag2;
    let dy = y.y - proj * k.y;
    let dz = y.z - proj * k.z;
    let perp = sqrt(dy * dy + dz * dz) / sqrt(kmag2);
    let keyness = clamp(proj, 0.0, 1.5) * (1.0 - smoothstep_fx(0.0, u.p1.x, perp));
    var m = clamp(1.0 - keyness * u.p1.y, 0.0, 1.0);
    m += max(y.x - k.x, 0.0) * u.p1.z * 2.0 * min(keyness, 1.0);
    m += max(k.x - y.x, 0.0) * u.p1.w * 2.0 * min(keyness, 1.0);
    let ped = u.p2.x;
    return clamp((clamp(m, 0.0, 1.0) - ped) / (1.0 - ped), 0.0, 1.0);
}

fn ukey_channel(v: vec3<f32>, i: u32) -> f32 {
    return select(select(v.x, v.y, i == 1u), v.z, i == 2u);
}

// The combine: `aux` is the original, `src` the cleaned-up matte (red) when i1.w is set.
fn ukey_combine(p: vec2<i32>) -> vec4<f32> {
    let o = textureLoad(aux, p, 0);
    var a = select(ukey_matte(o), ld(p).x, u.i1.w != 0u);
    let contrast = u.p2.z;
    if contrast > 0.0 {
        let g = 1.0 + contrast * 4.0;
        let mid = u.p2.w;
        a = clamp((a - mid) * g + mid, 0.0, 1.0);
    }
    let src_a = o.a;
    var v = enc(select(o.rgb / o.a, vec3(0.0), o.a <= 1e-6));
    let dom = u.i1.x;
    let o1 = ukey_channel(v, (dom + 1u) % 3u);
    let o2 = ukey_channel(v, (dom + 2u) % 3u);
    let spill = u.p3.x; let desat = u.p3.y; let range = u.p3.z; let sluma = u.p3.w;
    let limit = max(o1, o2) * (1.0 - range) + (o1 + o2) * 0.5 * range;
    let excess = max(ukey_channel(v, dom) - limit, 0.0);
    if excess > 0.0 && spill > 0.0 {
        let l0 = luma709(v);
        let one = vec3(select(0.0, 1.0, dom == 0u), select(0.0, 1.0, dom == 1u), select(0.0, 1.0, dom == 2u));
        v = v - one * (excess * spill);
        let l1 = luma709(v);
        v = l1 + (v - l1) * (1.0 - desat * min(excess * 4.0, 1.0));
        v = v + (l0 - l1) * sluma;
    }
    if u.i1.z != 0u {
        var h = rgb_to_hsl(clamp(v, vec3(0.0), vec3(1.0)));
        h.x = fract_euclid(h.x + u.p4.y);
        h.y = clamp(h.y * u.p4.x, 0.0, 1.0);
        h.z = clamp(h.z * u.p4.z, 0.0, 1.0);
        v = hsl_to_rgb(h.x, h.y, h.z);
    }
    let l = dec(v);
    switch u.i1.y {
        case 1u: {
            let g = srgb_to_linear1(a);
            return vec4(vec3(g * src_a), src_a);
        }
        case 2u: {
            return vec4(l * src_a, src_a);
        }
        default: {
            let na = a * src_a;
            return vec4(l * na, na);
        }
    }
}

fn pixel(op: u32, p: vec2<i32>) -> vec4<f32> {
    let s = size();
    let pc = vec2<f32>(p) + 0.5;
    switch op {
        case OP_INVERT_ALPHA: {
            let o = ld(p);
            let a = o.a;
            let na = 1.0 - a;
            // (the CPU's threshold; `na / a` is only used when a > 1e-6)
            var k = 0.0;
            if a > 1e-6 {
                k = na / a;
            }
            let blend = u.p0.x;
            return vec4(o.rgb * k, na * (1.0 - blend) + a * blend);
        }
        case OP_ALPHA_ADJUST: {
            let o = ld(p);
            let c = select(o.rgb / o.a, vec3(0.0), o.a <= 1e-6);
            var a = select(o.a, 1.0, u.i1.x != 0u);
            if u.i1.y != 0u {
                a = 1.0 - a;
            }
            a = clamp(a * u.p0.x, 0.0, 1.0);
            if u.i1.z != 0u {
                return vec4(vec3(srgb_to_linear1(a)), 1.0);
            }
            return vec4(c * a, a);
        }
        case OP_UKEY_MATTE: {
            return vec4(ukey_matte(ld(p)), 0.0, 0.0, 0.0);
        }
        case OP_UKEY_CHOKE: {
            // src: the blurred matte, aux: the original
            let m0 = ukey_matte(textureLoad(aux, p, 0));
            let c = u.p2.y * 0.5;
            return vec4(min(m0, clamp((ld(p).x - c) / (1.0 - c), 0.0, 1.0)), 0.0, 0.0, 0.0);
        }
        case OP_UKEY: {
            return ukey_combine(p);
        }
        case OP_COLOR_KEY: {
            // `FxOp::ColorKey`: key Y′CbCr = p0.xyz, tolerance p0.w, softness p1.x
            let o = ld(p);
            if o.a <= 0.0 {
                return o;
            }
            let c = enc(o.rgb / o.a);
            let y = ukey_ycbcr(c);
            let k = u.p0.xyz;
            let dc = y.yz - k.yz;
            let d = sqrt(dc.x * dc.x + dc.y * dc.y) + abs(y.x - k.x) * 0.15;
            let a = o.a * clamp((d - u.p0.w) / u.p1.x, 0.0, 1.0);
            return vec4(dec(c) * a, a);
        }
        case OP_LUMA_KEY: {
            // `FxOp::LumaKey`: th = p0.x, cut = p0.y
            let o = ld(p);
            if o.a <= 0.0 {
                return o;
            }
            let th = u.p0.x; let cut = u.p0.y;
            let l = linear_to_srgb1(max(luma709(o.rgb / o.a), 0.0));
            var k = (l - cut) / max(th - cut, 1e-3);
            if l <= cut {
                k = 0.0;
            } else if l >= max(th, cut + 1e-3) {
                k = 1.0;
            }
            return o * k;
        }
        case OP_BOX: {
            // one box pass along x (i1.z = 0) or y (1): mean over 2r+1 pixels, clamped to the
            // edge (repeat) or skipping pixels outside, as `effects::box_rows`
            let r = i32(u.i1.x);
            let repeat = u.i1.y != 0u;
            let vertical = u.i1.z != 0u;
            let n = select(s.x, s.y, vertical);
            let x = select(p.x, p.y, vertical);
            var acc = vec4(0.0);
            for (var i = x - r; i <= x + r; i++) {
                if repeat || (i >= 0 && i < n) {
                    let j = clamp(i, 0, n - 1);
                    acc += ld(select(vec2(j, p.y), vec2(p.x, j), vertical));
                }
            }
            return acc * (1.0 / f32(2 * r + 1));
        }
        case OP_MEDIAN_MASK: {
            // `effects::median_filter` of a mask (in every channel) over the (2r+1)² window
            // clipped to the image, mixed in by p0.x (HSL Secondary ▸ Denoise)
            let r = i32(u.i1.x);
            var win: array<f32, 49>;
            var n = 0u;
            for (var y = max(p.y - r, 0); y <= min(p.y + r, s.y - 1); y++) {
                for (var x = max(p.x - r, 0); x <= min(p.x + r, s.x - 1); x++) {
                    if n < 49u {
                        win[n] = ld(vec2(x, y)).x;
                        n++;
                    }
                }
            }
            let k = n / 2u;
            for (var i = 0u; i <= k; i++) {
                var lo = i;
                for (var j = i + 1u; j < n; j++) {
                    if win[j] < win[lo] {
                        lo = j;
                    }
                }
                let t = win[i];
                win[i] = win[lo];
                win[lo] = t;
            }
            let m = ld(p).x;
            return vec4(m + (win[k] - m) * min(u.p0.x, 1.0));
        }
        case OP_DIRECTIONAL: {
            let steps = u.i1.x;
            let dx = u.p0.x; let dy = u.p0.y;
            var acc = vec4(0.0);
            for (var i = 0u; i < steps; i++) {
                let t = f32(i) / f32(steps - 1u) - 0.5;
                acc += sample_clamped(pc.x + dx * t, pc.y + dy * t);
            }
            return acc / f32(steps);
        }
        case OP_DENOISE_YCC, OP_TEMPORAL_INIT: {
            return denoise_ycc(p);
        }
        case OP_DENOISE: {
            return denoise_px(p);
        }
        case OP_UNSHARP: {
            // src: the blurred image, aux: the original
            return unsharp_mix(textureLoad(aux, p, 0), ld(p));
        }
        case OP_UNSHARP_DIRECT: {
            // Unsharp with at most one box per axis in one pass (fork X422 2b.2, `fx.rs`
            // `fused_steps`; src: the original): the blur summed in the order of the two box passes
            // (each row's mean, then the mean of those down the column), clamped to the edge
            let rx = min(i32(u.i1.x), 3);
            let ry = min(i32(u.i1.y), 3);
            let inv_x = 1.0 / f32(2 * rx + 1);
            let inv_y = 1.0 / f32(2 * ry + 1);
            var acc = vec4(0.0);
            for (var j = p.y - ry; j <= p.y + ry; j++) {
                let y = clamp(j, 0, s.y - 1);
                var row = vec4(0.0);
                for (var i = p.x - rx; i <= p.x + rx; i++) {
                    row += ld(vec2(clamp(i, 0, s.x - 1), y));
                }
                acc += row * inv_x;
            }
            return unsharp_mix(ld(p), acc * inv_y);
        }
        case OP_CROP: {
            let x0 = u.p0.x; let x1 = u.p0.y; let y0 = u.p0.z; let y1 = u.p0.w; let fe = u.p1.x;
            let d = min(min(pc.x - x0, x1 - pc.x), min(pc.y - y0, y1 - pc.y));
            var a: f32;
            if fe > 0.0 {
                a = clamp(d / fe, 0.0, 1.0);
            } else {
                a = clamp(d + 0.5, 0.0, 1.0);
            }
            let o = ld(p);
            return select(o, o * a, a < 1.0);
        }
        case OP_RESAMPLE: {
            // inverse map p0 = (a b c d), p1 = (e f opacity valid); rect i1 = x0 x1 y0 y1
            if u.p1.w == 0.0 || u32(p.x) < u.i1.x || u32(p.x) >= u.i1.y || u32(p.y) < u.i1.z || u32(p.y) >= u.i1.w {
                return vec4(0.0);
            }
            let m0 = u.p0; let m1 = u.p1;
            let uu = m0.x * pc.x + m0.z * pc.y + m1.x;
            let vv = m0.y * pc.x + m0.w * pc.y + m1.y;
            if uu < -1.0 || vv < -1.0 || uu > f32(s.x) + 1.0 || vv > f32(s.y) + 1.0 {
                return vec4(0.0);
            }
            return sample_bilinear(uu, vv) * m1.z;
        }
        case OP_HFLIP: {
            return ld(vec2(s.x - 1 - p.x, p.y));
        }
        case OP_VFLIP: {
            return ld(vec2(p.x, s.y - 1 - p.y));
        }
        case OP_MIRROR: {
            let c = u.p0.xy; let nrm = u.p0.zw;
            let d = (pc.x - c.x) * nrm.x + (pc.y - c.y) * nrm.y;
            if d > 0.0 {
                return sample_bilinear(pc.x - 2.0 * d * nrm.x, pc.y - 2.0 * d * nrm.y);
            }
            return ld(p);
        }
        case OP_OFFSET: {
            let w = f32(s.x); let h = f32(s.y);
            // dx, dy in 0..w / 0..h, so the shifted coordinate is within (-w, w + 0.5)
            var uu = pc.x - u.p0.x;
            var vv = pc.y - u.p0.y;
            if uu < 0.0 { uu += w; }
            if uu >= w { uu -= w; }
            if vv < 0.0 { vv += h; }
            if vv >= h { vv -= h; }
            let q = sample_clamped(uu, vv);
            let o = ld(p);
            return q + (o - q) * u.p0.z;
        }
        case OP_VIGNETTE: {
            let o = ld(p);
            let a = o.a;
            if a <= 1e-6 {
                return o;
            }
            let amt = u.p0.x;
            if abs(amt) < 1e-5 {
                return o;
            }
            let mid = u.p0.y;
            let round = u.p0.z;
            let feather = u.p0.w;
            let tgt = u.p1.xyz;
            let w = f32(s.x);
            let h = f32(s.y);
            let aspect = w / h;
            var nx = pc.x / w * 2.0 - 1.0;
            let ny = pc.y / h * 2.0 - 1.0;
            if round > 0.0 {
                nx *= 1.0 + (aspect - 1.0) * round;
            }
            let p_exp = select(2.0, 2.0 + (-round) * 6.0, round < 0.0);
            let d = powf(powf(abs(nx), p_exp) + powf(abs(ny), p_exp), 1.0 / p_exp) / powf(2.0, 1.0 / p_exp);
            let edge = smoothstep_fx(mid - feather * 0.5, mid + feather * 0.5, d);
            let c = o.rgb / a;
            let ec = enc(c);
            let res = dec(ec + (tgt - ec) * (edge * abs(amt)));
            return vec4(res * a, a);
        }
        case OP_VIDEO_LIMITER: {
            let o = ld(p);
            let a = o.a;
            if a <= 1e-6 {
                return o;
            }
            let c = o.rgb / a;
            let v = vec3(linear_to_enc_limiter(c.x), linear_to_enc_limiter(c.y), linear_to_enc_limiter(c.z));
            let max_val = u.p0.x;
            let comp = u.p0.y;
            let axis = u.i1.x;
            let warn = u.i1.y != 0u;
            let wc = u.p0.zw;
            let wc_b = u.p1.x;
            let warning_col = vec3(wc.x, wc.y, wc_b);
            let out_enc = limit_video(v, max_val, comp, axis);
            let diff = abs(v - out_enc);
            if warn && (diff.x > 1e-4 || diff.y > 1e-4 || diff.z > 1e-4) {
                return vec4(warning_col * a, a);
            }
            let res = vec3(enc_to_linear_limiter(out_enc.x), enc_to_linear_limiter(out_enc.y), enc_to_linear_limiter(out_enc.z));
            return vec4(res * a, a);
        }
        default: {
            let o = ld(p);
            let a = o.a;
            if a <= 1e-6 {
                return o;
            }
            return vec4(color_op(op, o.rgb / a) * a, a);
        }
    }
}

// The op of a pipeline built for one op (`fx.rs` `px_pipeline`; 0: the op in the uniforms). A
// constant op lets the compiler drop the other cases of `pixel` (fork U0.2.3b: the whole switch
// costs Intel's iGPU ~50 ms per pass at 6.2K).
override PX_OP: u32 = 0u;

@compute @workgroup_size(16, 16)
fn fx_px(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= u.i0.y || id.y >= u.i0.z {
        return;
    }
    let p = vec2<i32>(id.xy);
    textureStore(dst, p, pixel(select(u.i0.x, PX_OP, PX_OP != 0u), p));
}

// Up to three box passes along one axis in one dispatch (fork X422 2b.1, `fx.rs` `fuse_boxes`):
// i1.x = the radii, 8 bits each (each ≤ BOX_PER_PIXEL_MAX), i1.y = repeat, i1.z = vertical,
// i1.w = how many. A workgroup does one tile of BOX3_TILE pixels of one row (column): it loads the
// tile plus an apron of the radii's sum into shared memory, runs the boxes there and stores the
// tile. Each box reads its predecessor at image coordinates, clamped (repeat) or skipped like
// OP_BOX, summed in the same order, so the result is the cascade of OP_BOX passes.
const BOX3_TILE: i32 = 256;
// BOX3_TILE + 2 × 3 × BOX_PER_PIXEL_MAX
const BOX3_SPAN: i32 = 448;
var<workgroup> box3_a: array<vec4<f32>, 448>;
var<workgroup> box3_b: array<vec4<f32>, 448>;

fn box3_radius(k: u32) -> i32 {
    return min(i32((u.i1.x >> (8u * k)) & 0xffu), 32);
}

fn box3_ld(from_b: bool, i: i32) -> vec4<f32> {
    let j = clamp(i, 0, BOX3_SPAN - 1);
    if from_b {
        return box3_b[j];
    }
    return box3_a[j];
}

@compute @workgroup_size(256)
fn fx_box3(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    let s = size();
    let vertical = u.i1.z != 0u;
    let repeat = u.i1.y != 0u;
    let count = min(u.i1.w, 3u);
    let n = select(s.x, s.y, vertical);
    let lines = select(s.y, s.x, vertical);
    let line = i32(wg.y);
    let t0 = i32(wg.x) * BOX3_TILE;
    // (uniform per workgroup, so before any barrier)
    if line >= lines || t0 >= n || count == 0u {
        return;
    }
    var apron = 0;
    for (var k = 0u; k < count; k++) {
        apron += box3_radius(k);
    }
    // image coordinate of shared index 0
    let base = t0 - apron;
    let hi0 = min(t0 + BOX3_TILE + apron, n);
    for (var x = max(base, 0) + i32(li); x < hi0; x += BOX3_TILE) {
        box3_a[clamp(x - base, 0, BOX3_SPAN - 1)] = ld(select(vec2(x, line), vec2(line, x), vertical));
    }
    workgroupBarrier();
    var rem = apron;
    for (var k = 0u; k < count; k++) {
        let r = box3_radius(k);
        rem -= r;
        let from_b = (k & 1u) == 1u;
        let last = k + 1u == count;
        let hi = min(t0 + BOX3_TILE + rem, n);
        let inv = 1.0 / f32(2 * r + 1);
        for (var x = max(t0 - rem, 0) + i32(li); x < hi; x += BOX3_TILE) {
            var acc = vec4(0.0);
            for (var i = x - r; i <= x + r; i++) {
                if repeat || (i >= 0 && i < n) {
                    acc += box3_ld(from_b, clamp(i, 0, n - 1) - base);
                }
            }
            let v = acc * inv;
            if last {
                textureStore(dst, select(vec2(x, line), vec2(line, x), vertical), v);
            } else if from_b {
                box3_a[clamp(x - base, 0, BOX3_SPAN - 1)] = v;
            } else {
                box3_b[clamp(x - base, 0, BOX3_SPAN - 1)] = v;
            }
        }
        workgroupBarrier();
    }
}

// A box pass as a running sum along one row (i1.z = 0) or column (1) per invocation: O(1) per
// pixel for any radius, the arithmetic of `effects::box_rows`.
@compute @workgroup_size(64)
fn fx_run(@builtin(global_invocation_id) id: vec3<u32>) {
    let s = size();
    let vertical = u.i1.z != 0u;
    let n = select(s.x, s.y, vertical);
    let lines = select(s.y, s.x, vertical);
    let line = i32(id.x);
    if line >= lines {
        return;
    }
    let r = i32(u.i1.x);
    let repeat = u.i1.y != 0u;
    let inv = 1.0 / f32(2 * r + 1);
    var acc = vec4(0.0);
    for (var i = -r; i <= r; i++) {
        if repeat || (i >= 0 && i < n) {
            let j = clamp(i, 0, n - 1);
            acc += ld(select(vec2(j, line), vec2(line, j), vertical));
        }
    }
    for (var x = 0; x < n; x++) {
        textureStore(dst, select(vec2(x, line), vec2(line, x), vertical), acc * inv);
        let out_i = x - r;
        let in_i = x + r + 1;
        if repeat || out_i >= 0 {
            let j = clamp(out_i, 0, n - 1);
            acc -= ld(select(vec2(j, line), vec2(line, j), vertical));
        }
        if repeat || in_i < n {
            let j = clamp(in_i, 0, n - 1);
            acc += ld(select(vec2(j, line), vec2(line, j), vertical));
        }
    }
}

// ---- temporal noise reduction (`filmcraft_render::tnr`, fork 6.7)
//
// `OP_TEMPORAL_INIT` (a `pixel` op) converts the current image and each neighbour to
// (Y, Cb, Cr, α) like `denoise_ycc`. `temporal_accumulate` then reads the current one as `src`,
// the original as `aux` and, in group 1, up to four neighbours with their forward (current →
// neighbour) and backward flows (`Rg32Float`, grid pixels), and writes the finished pixel.

struct TU {
    n: vec4<u32>,              // neighbours, luma on, chroma on, unused
    k: vec4<f32>,              // 1/2σl², 1/2σc², 1/τ² (agreement), unused
    gi: array<vec4<u32>, 4>,   // flow grid size per neighbour
    gs: array<vec4<f32>, 4>,   // image pixels per grid pixel (x, y) per neighbour
};

@group(1) @binding(0) var<uniform> tu: TU;
@group(1) @binding(1) var tn0: texture_2d<f32>;
@group(1) @binding(2) var tn1: texture_2d<f32>;
@group(1) @binding(3) var tn2: texture_2d<f32>;
@group(1) @binding(4) var tn3: texture_2d<f32>;
@group(1) @binding(5) var tf0: texture_2d<f32>;
@group(1) @binding(6) var tb0: texture_2d<f32>;
@group(1) @binding(7) var tf1: texture_2d<f32>;
@group(1) @binding(8) var tb1: texture_2d<f32>;
@group(1) @binding(9) var tf2: texture_2d<f32>;
@group(1) @binding(10) var tb2: texture_2d<f32>;
@group(1) @binding(11) var tf3: texture_2d<f32>;
@group(1) @binding(12) var tb3: texture_2d<f32>;

fn flow_at(t: texture_2d<f32>, g: vec2<i32>, x: i32, y: i32) -> vec2<f32> {
    return textureLoad(t, vec2(clamp(x, 0, g.x - 1), clamp(y, 0, g.y - 1)), 0).xy;
}

// `FlowField::sample`: bilinear at grid coordinates, clamped.
fn flow_sample(t: texture_2d<f32>, g: vec2<i32>, x: f32, y: f32) -> vec2<f32> {
    let x0f = floor(x);
    let y0f = floor(y);
    let fx = x - x0f;
    let fy = y - y0f;
    let x0 = i32(x0f);
    let y0 = i32(y0f);
    let a = flow_at(t, g, x0, y0);
    let b = flow_at(t, g, x0 + 1, y0);
    let c = flow_at(t, g, x0, y0 + 1);
    let d = flow_at(t, g, x0 + 1, y0 + 1);
    let top = a + (b - a) * fx;
    let bot = c + (d - c) * fx;
    return top + (bot - top) * fy;
}

// `tnr::bilinear`: (Y, Cb, Cr, 1) at an image point (centres at +0.5, clamped); w = −1 when
// one of the four texels is a skipped pixel.
fn tnr_bilinear(t: texture_2d<f32>, x: f32, y: f32) -> vec4<f32> {
    let s = size();
    let fx = clamp(x - 0.5, 0.0, f32(s.x - 1));
    let fy = clamp(y - 0.5, 0.0, f32(s.y - 1));
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let x1 = min(x0 + 1, s.x - 1);
    let y1 = min(y0 + 1, s.y - 1);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let a = textureLoad(t, vec2(x0, y0), 0);
    let b = textureLoad(t, vec2(x1, y0), 0);
    let c = textureLoad(t, vec2(x0, y1), 0);
    let d = textureLoad(t, vec2(x1, y1), 0);
    if a.w <= 0.0 || b.w <= 0.0 || c.w <= 0.0 || d.w <= 0.0 {
        return vec4(0.0, 0.0, 0.0, -1.0);
    }
    let top = a.xyz + (b.xyz - a.xyz) * tx;
    let bot = c.xyz + (d.xyz - c.xyz) * tx;
    return vec4(top + (bot - top) * ty, 1.0);
}

// `tnr::bilinear_y`
fn tnr_bilinear_y(t: texture_2d<f32>, x: f32, y: f32) -> f32 {
    let s = size();
    let fx = clamp(x - 0.5, 0.0, f32(s.x - 1));
    let fy = clamp(y - 0.5, 0.0, f32(s.y - 1));
    let x0 = i32(floor(fx));
    let y0 = i32(floor(fy));
    let x1 = min(x0 + 1, s.x - 1);
    let y1 = min(y0 + 1, s.y - 1);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let a = textureLoad(t, vec2(x0, y0), 0).x;
    let b = textureLoad(t, vec2(x1, y0), 0).x;
    let c = textureLoad(t, vec2(x0, y1), 0).x;
    let d = textureLoad(t, vec2(x1, y1), 0).x;
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    return top + (bot - top) * ty;
}

struct TC {
    wl: f32,
    wc: f32,
    v: vec3<f32>,
};

// `tnr::contribution`: one neighbour's weights and value at pixel `p` (zero weights: nothing).
fn tnr_contrib(nb: texture_2d<f32>, fw: texture_2d<f32>, bw: texture_2d<f32>, gi: vec4<u32>, gs: vec4<f32>, p: vec2<i32>, c0: vec4<f32>) -> TC {
    var o = TC(0.0, 0.0, vec3(0.0));
    let s = size();
    let g = vec2<i32>(i32(max(gi.x, 1u)), i32(max(gi.y, 1u)));
    let px = f32(p.x) + 0.5;
    let py = f32(p.y) + 0.5;
    let sx = gs.x;
    let sy = gs.y;
    let gx = px / sx - 0.5;
    let gy = py / sy - 0.5;
    let f = flow_sample(fw, g, gx, gy);
    let qx = px + f.x * sx;
    let qy = py + f.y * sy;
    if !(qx >= 0.0 && qx <= f32(s.x) && qy >= 0.0 && qy <= f32(s.y)) {
        return o;
    }
    let r = flow_sample(bw, g, gx + f.x, gy + f.y);
    let ex = f.x + r.x;
    let ey = f.y + r.y;
    let fb2 = ex * ex + ey * ey;
    let agree = 1.0 / (1.0 + fb2 * tu.k.z);
    let c = tnr_bilinear(nb, qx, qy);
    if c.w < 0.0 {
        return o;
    }
    var sad = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        let yy = clamp(p.y + dy, 0, s.y - 1);
        for (var dx = -1; dx <= 1; dx++) {
            let xx = clamp(p.x + dx, 0, s.x - 1);
            let yt = ld(vec2(xx, yy)).x;
            let yk = tnr_bilinear_y(nb, qx + f32(dx), qy + f32(dy));
            sad += abs(yt - yk);
        }
    }
    let d = sad / 9.0;
    if tu.n.y != 0u {
        o.wl = exp(-(d * d) * tu.k.x) * agree;
    }
    if tu.n.z != 0u {
        let d1 = c.y - c0.y;
        let d2 = c.z - c0.z;
        o.wc = exp(-(d1 * d1 + d2 * d2) * tu.k.y) * agree;
    }
    o.v = c.xyz;
    return o;
}

// `tnr::apply_with_flows` for one pixel: src = the current (Y, Cb, Cr, α), aux = the original.
@compute @workgroup_size(16, 16)
fn temporal_accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= u.i0.y || id.y >= u.i0.z {
        return;
    }
    let p = vec2<i32>(id.xy);
    let c0 = ld(p);
    let orig = textureLoad(aux, p, 0);
    if c0.w <= 0.0 {
        textureStore(dst, p, orig);
        return;
    }
    var sl = 0.0;
    var wl = 0.0;
    var sc = vec2(0.0);
    var wc = 0.0;
    let n = tu.n.x;
    if n > 0u {
        let t = tnr_contrib(tn0, tf0, tb0, tu.gi[0], tu.gs[0], p, c0);
        sl += t.wl * t.v.x;
        wl += t.wl;
        sc += t.wc * t.v.yz;
        wc += t.wc;
    }
    if n > 1u {
        let t = tnr_contrib(tn1, tf1, tb1, tu.gi[1], tu.gs[1], p, c0);
        sl += t.wl * t.v.x;
        wl += t.wl;
        sc += t.wc * t.v.yz;
        wc += t.wc;
    }
    if n > 2u {
        let t = tnr_contrib(tn2, tf2, tb2, tu.gi[2], tu.gs[2], p, c0);
        sl += t.wl * t.v.x;
        wl += t.wl;
        sc += t.wc * t.v.yz;
        wc += t.wc;
    }
    if n > 3u {
        let t = tnr_contrib(tn3, tf3, tb3, tu.gi[3], tu.gs[3], p, c0);
        sl += t.wl * t.v.x;
        wl += t.wl;
        sc += t.wc * t.v.yz;
        wc += t.wc;
    }
    var yy = c0.x;
    if tu.n.y != 0u {
        yy = (c0.x + sl) / (1.0 + wl);
    }
    var cc = c0.yz;
    if tu.n.z != 0u {
        cc = (c0.yz + sc) / (1.0 + wc);
    }
    let out = vec4(nr_rgb(vec3(yy, cc)) * c0.w, c0.w);
    if !finite4(out) {
        textureStore(dst, p, orig);
        return;
    }
    // (alpha: the original's, bit for bit)
    textureStore(dst, p, vec4(out.rgb, orig.a));
}

// Bitmap matte mix (fork G3.2, `FxOp::MaskedBy`; CPU reference `filmcraft_render::matte`): the
// effected image (`src`) mixed with the image before the effect (`aux`) by the matte (`R8Unorm`,
// any size) stretched over the frame and sampled bilinearly at the pixel centre, edges clamped.
// The interpolation is done here in f32 (not by a sampler) so it matches the CPU's.
@group(1) @binding(13) var matte: texture_2d<f32>;

fn matte_tap(n: u32, m: u32, i: u32) -> vec3<f32> {
    let last = f32(m - 1u);
    let c = clamp((f32(i) + 0.5) * (f32(m) / f32(max(n, 1u))) - 0.5, 0.0, last);
    let a = min(floor(c), last);
    let b = min(a + 1.0, last);
    return vec3<f32>(a, b, c - a);
}

@compute @workgroup_size(16, 16)
fn matte_mix(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = u.i0.y;
    let h = u.i0.z;
    if (id.x >= w || id.y >= h) {
        return;
    }
    let md = textureDimensions(matte);
    let tx = matte_tap(w, md.x, id.x);
    let ty = matte_tap(h, md.y, id.y);
    let x0 = i32(tx.x);
    let x1 = i32(tx.y);
    let y0 = i32(ty.x);
    let y1 = i32(ty.y);
    let m00 = textureLoad(matte, vec2<i32>(x0, y0), 0).r;
    let m10 = textureLoad(matte, vec2<i32>(x1, y0), 0).r;
    let m01 = textureLoad(matte, vec2<i32>(x0, y1), 0).r;
    let m11 = textureLoad(matte, vec2<i32>(x1, y1), 0).r;
    let top = m00 + (m10 - m00) * tx.z;
    let bot = m01 + (m11 - m01) * tx.z;
    let cov = top + (bot - top) * ty.z;
    let e = textureLoad(src, vec2<i32>(id.xy), 0);
    let o = textureLoad(aux, vec2<i32>(id.xy), 0);
    textureStore(dst, vec2<i32>(id.xy), o + (e - o) * cov);
}
