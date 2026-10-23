// Projective warp of comp programs (fork C3.8): the GPU twin of
// `filmcraft_render::comp::track::warp_projective`. Every output pixel centre is mapped through
// the inverse homography and the source sampled bilinearly with a transparent border, exactly as
// `Image::sample_bilinear`; minified maps first halve the source (`fs_half`, as
// `Image::downsample2`) as often as the CPU does (the levels are chosen on the CPU in f64).

struct WarpParams {
    // the inverse homography's rows (destination pixels -> source pixels of the sampled level)
    r0: vec4<f32>,
    r1: vec4<f32>,
    r2: vec4<f32>,
    // (source width, source height, -, -) of the sampled level
    size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> wp: WarpParams;
@group(0) @binding(1) var src: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}

// A texel, transparent outside the picture (`Image::get_or_clear`).
fn texel_or_clear(x: i32, y: i32, dims: vec2<i32>) -> vec4<f32> {
    if x < 0 || y < 0 || x >= dims.x || y >= dims.y {
        return vec4(0.0);
    }
    return textureLoad(src, vec2(x, y), 0);
}

@fragment
fn fs_warp(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // (pos.xy is the pixel centre, x + 0.5)
    let p = vec3(pos.xy, 1.0);
    let w = dot(wp.r2.xyz, p);
    // `Homography::apply`: nothing at (or near) the line sent to infinity; NaN fails every test
    if !(abs(w) >= 1e-12) || !(abs(w) < 3.0e38) {
        return vec4(0.0);
    }
    let s = vec2(dot(wp.r0.xyz, p), dot(wp.r1.xyz, p)) / w;
    // outside the source by more than a pixel (and non-finite: every comparison fails)
    if !(s.x >= -1.0 && s.y >= -1.0 && s.x <= wp.size.x + 1.0 && s.y <= wp.size.y + 1.0) {
        return vec4(0.0);
    }
    let dims = vec2<i32>(textureDimensions(src));
    let f = s - 0.5;
    let f0 = floor(f);
    let t = f - f0;
    let i0 = vec2<i32>(f0);
    let a = texel_or_clear(i0.x, i0.y, dims);
    let b = texel_or_clear(i0.x + 1, i0.y, dims);
    let c = texel_or_clear(i0.x, i0.y + 1, dims);
    let d = texel_or_clear(i0.x + 1, i0.y + 1, dims);
    let top = a + (b - a) * t.x;
    let bot = c + (d - c) * t.x;
    return top + (bot - top) * t.y;
}

// A texel clamped to the picture (`Image::get_clamped`).
fn texel_clamped(x: i32, y: i32, dims: vec2<i32>) -> vec4<f32> {
    return textureLoad(src, clamp(vec2(x, y), vec2(0), dims - 1), 0);
}

// One half-size level: the mean of each 2 x 2 block (`Image::downsample2`).
@fragment
fn fs_half(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(src));
    let o = vec2<i32>(pos.xy) * 2;
    let a = texel_clamped(o.x, o.y, dims);
    let b = texel_clamped(o.x + 1, o.y, dims);
    let c = texel_clamped(o.x, o.y + 1, dims);
    let d = texel_clamped(o.x + 1, o.y + 1, dims);
    return (a + b + c + d) * 0.25;
}
