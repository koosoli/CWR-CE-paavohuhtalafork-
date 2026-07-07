// HDR bloom — energy-conserving dual-filter pyramid (Jimenez, "Next Generation Post
// Processing in Call of Duty: Advanced Warfare"). A downsample chain builds a mip
// pyramid of the linear HDR scene; a tent upsample chain sums them back up, spreading
// bright light into a wide, stable glow. The tonemap resolve then adds mip0 to the
// scene before exposure. Three fragment entry points share one bind layout:
//   fs_prefilter : scene -> mip0, with a soft-knee threshold + Karis firefly average
//   fs_downsample: mip i -> mip i+1, 13-tap box
//   fs_upsample  : mip i+1 -> mip i, 3x3 tent (additively blended)
// Source texel size is read from textureDimensions, so no per-pass uniform is needed.

struct BloomParams {
    threshold: f32, // luminance knee centre (scene-referred)
    knee: f32,      // soft-knee half-width
    intensity: f32, // used by the tonemap composite, not here
    radius: f32,    // upsample tent radius (texels)
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;
@group(0) @binding(2) var<uniform> bloom: BloomParams;

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.uv = uv;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

fn s(uv: vec2<f32>) -> vec3<f32> {
    return max(textureSampleLevel(src_tex, src_samp, uv, 0.0).rgb, vec3<f32>(0.0));
}

fn src_texel() -> vec2<f32> {
    return 1.0 / vec2<f32>(textureDimensions(src_tex, 0));
}

// Karis average: weight a group by 1/(1+luma) so a single firefly can't dominate.
fn karis_avg(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    let wa = 1.0 / (1.0 + dot(a, LUMA));
    let wb = 1.0 / (1.0 + dot(b, LUMA));
    let wc = 1.0 / (1.0 + dot(c, LUMA));
    let wd = 1.0 / (1.0 + dot(d, LUMA));
    return (a * wa + b * wb + c * wc + d * wd) / (wa + wb + wc + wd);
}

// The 13 taps of the CoD downsample, arranged as an outer 3x3 (a..i) plus an inner
// 2x2 (j..m). `karis` splits them into five 2x2 boxes (used only on the prefilter).
fn downsample13(uv: vec2<f32>, texel: vec2<f32>, karis: bool) -> vec3<f32> {
    let a = s(uv + texel * vec2<f32>(-2.0, -2.0));
    let b = s(uv + texel * vec2<f32>( 0.0, -2.0));
    let c = s(uv + texel * vec2<f32>( 2.0, -2.0));
    let d = s(uv + texel * vec2<f32>(-2.0,  0.0));
    let e = s(uv + texel * vec2<f32>( 0.0,  0.0));
    let f = s(uv + texel * vec2<f32>( 2.0,  0.0));
    let g = s(uv + texel * vec2<f32>(-2.0,  2.0));
    let h = s(uv + texel * vec2<f32>( 0.0,  2.0));
    let i = s(uv + texel * vec2<f32>( 2.0,  2.0));
    let j = s(uv + texel * vec2<f32>(-1.0, -1.0));
    let k = s(uv + texel * vec2<f32>( 1.0, -1.0));
    let l = s(uv + texel * vec2<f32>(-1.0,  1.0));
    let m = s(uv + texel * vec2<f32>( 1.0,  1.0));

    if (karis) {
        // Five boxes, Karis-averaged, combined 0.5 (centre) + 0.125 (corners).
        var o = karis_avg(j, k, l, m) * 0.5;
        o += karis_avg(a, b, d, e) * 0.125;
        o += karis_avg(b, c, e, f) * 0.125;
        o += karis_avg(d, e, g, h) * 0.125;
        o += karis_avg(e, f, h, i) * 0.125;
        return o;
    }
    var o = e * 0.125;
    o += (a + c + g + i) * 0.03125;
    o += (b + d + f + h) * 0.0625;
    o += (j + k + l + m) * 0.125;
    return o;
}

// Soft-knee threshold (Unreal-style quadratic knee): passes bright light, rolls off
// smoothly below the threshold so there is no hard cut-off shimmer.
fn prefilter(c: vec3<f32>) -> vec3<f32> {
    let br = max(c.r, max(c.g, c.b));
    let knee = max(bloom.knee, 1e-4);
    let soft = clamp(br - bloom.threshold + knee, 0.0, 2.0 * knee);
    let curve = soft * soft / (4.0 * knee);
    let contrib = max(curve, br - bloom.threshold) / max(br, 1e-4);
    return c * contrib;
}

@fragment
fn fs_prefilter(in: VsOut) -> @location(0) vec4<f32> {
    let c = downsample13(in.uv, src_texel(), true);
    return vec4<f32>(prefilter(c), 1.0);
}

@fragment
fn fs_downsample(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(downsample13(in.uv, src_texel(), false), 1.0);
}

@fragment
fn fs_upsample(in: VsOut) -> @location(0) vec4<f32> {
    let texel = src_texel() * bloom.radius;
    var o = s(in.uv + texel * vec2<f32>(-1.0, -1.0)) * 1.0;
    o += s(in.uv + texel * vec2<f32>( 0.0, -1.0)) * 2.0;
    o += s(in.uv + texel * vec2<f32>( 1.0, -1.0)) * 1.0;
    o += s(in.uv + texel * vec2<f32>(-1.0,  0.0)) * 2.0;
    o += s(in.uv + texel * vec2<f32>( 0.0,  0.0)) * 4.0;
    o += s(in.uv + texel * vec2<f32>( 1.0,  0.0)) * 2.0;
    o += s(in.uv + texel * vec2<f32>(-1.0,  1.0)) * 1.0;
    o += s(in.uv + texel * vec2<f32>( 0.0,  1.0)) * 2.0;
    o += s(in.uv + texel * vec2<f32>( 1.0,  1.0)) * 1.0;
    return vec4<f32>(o / 16.0, 1.0);
}
