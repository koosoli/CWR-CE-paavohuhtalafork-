// cam_above is the camera's height above the local water surface in metres (negative = the eye is
// submerged). inv_view_proj unprojects a forward-NDC point to a CAMERA-RELATIVE world position,
// exactly as Frame.inv_view_proj does — it is inverted view-and-proj-separately in f64 on the Rust
// side, because the reversed-Z infinite-far projection is ill-conditioned to invert in f32.
struct Params {
    time: f32,
    cam_above: f32,
    _pad: vec2<f32>,
    inv_view_proj: mat4x4<f32>,
    body_color_ext: vec4<f32>, // xyz = water deep body colour (gamma), w = extinction (1/m)
};

// Distance assigned to a cleared/far depth, and the ceiling on any path length: far enough
// that transmittance has saturated, so the exact value cannot show.
const FAR_PATH_M: f32 = 120.0;
// Radiance of light scattered back out of the volume. The one magnitude this model cannot get
// from an authored value — the deep colour supplies the hue (see below).
const INSCATTER_RADIANCE: f32 = 0.16;
const CAUSTIC_STRENGTH: f32 = 0.055;

fn srgb_to_linear_v3(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}
@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_samp: sampler;
@group(0) @binding(2) var scene_depth: texture_depth_2d;
@group(0) @binding(3) var<uniform> params: Params;

struct VsOut { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.uv = uv;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = textureDimensions(scene_depth);
    let dims_i = vec2<i32>(dims);
    let dims_f = vec2<f32>(dims);
    let pixel = clamp(vec2<i32>(in.clip.xy), vec2<i32>(0), dims_i - vec2<i32>(1));
    // Reversed-Z depth is proportional to 1/distance. The approximation deliberately
    // saturates for sky/cleared depth, producing dense distant underwater haze.
    let base_depth = textureLoad(scene_depth, pixel, 0);
    // Two broad travelling waves refract the completed scene beneath the surface.
    // Keep this in screen space and below three pixels so it reads as water volume,
    // not a camera shake.
    let wave = vec2<f32>(
        sin(in.uv.y * 17.0 + params.time * 1.10) + sin(in.uv.x * 9.0 - params.time * 0.63),
        sin(in.uv.x * 15.0 - params.time * 0.92) + sin(in.uv.y * 7.0 + params.time * 0.48)
    ) * 0.5;
    let warp_limit = 3.0 / dims_f;
    let warped_uv = clamp(in.uv + wave * warp_limit, vec2<f32>(0.001), vec2<f32>(0.999));
    let warped_pixel = clamp(vec2<i32>(warped_uv * dims_f), vec2<i32>(0), dims_i - vec2<i32>(1));
    let warped_depth = textureLoad(scene_depth, warped_pixel, 0);
    // Do not refract a closer foreground object over its background neighbour.
    let use_warp = warped_depth <= base_depth + 0.001;
    let sample_uv = select(in.uv, warped_uv, use_warp);
    let depth = select(base_depth, warped_depth, use_warp);
    let color = textureSampleLevel(scene_tex, scene_samp, sample_uv, 0.0).rgb;

    // TRUE path length, in metres. The old `0.12 / depth` was not a length at all: `depth` is
    // reversed-Z (proportional to 1/distance), so that expression was distance scaled by an
    // arbitrary constant, and every extinction coefficient below it had to be tuned against a
    // meaningless unit. `inv_view_proj` was already being uploaded for exactly this and was
    // never read. Unproject the pixel to a camera-relative position the way water.wgsl's
    // seabed_depth does (forward ndc.z = 1 - stored) and take its distance from the eye.
    // With the camera submerged the whole view ray is inside the water, so that distance IS
    // the path light travelled through it.
    let ndc_xy = vec2<f32>(sample_uv.x * 2.0 - 1.0, 1.0 - sample_uv.y * 2.0);
    var path_m = FAR_PATH_M;
    var world_rel = vec3<f32>(0.0);
    // depth ~ 0 is the reversed-Z far/cleared value: nothing was drawn down this ray, so the
    // unproject would divide by a ~0 w. Treat it as maximally far (dense haze), as before.
    if (depth > 1e-6) {
        let h = params.inv_view_proj * vec4<f32>(ndc_xy, 1.0 - depth, 1.0);
        world_rel = h.xyz / h.w;
        path_m = clamp(length(world_rel), 0.0, FAR_PATH_M);
    }

    // Beer-Lambert against the AUTHORED body colour and extinction, both of which the shader
    // previously ignored in favour of hardcoded constants — the same dead-control pattern that
    // made the surface unfixable for six passes. body_color_ext.xyz is authored in gamma, hence
    // the srgb decode that was defined here and never called.
    //
    // pow(deep, path*ext) is exp(-sigma*path) with the per-channel sigma implied by the authored
    // colour: after one extinction length the transmittance IS the authored deep colour. So the
    // Water tab's deep colour now sets what the water does to light, rather than being a swatch
    // that nothing reads.
    let deep_linear = srgb_to_linear_v3(clamp(params.body_color_ext.xyz, vec3<f32>(1e-4), vec3<f32>(1.0)));
    let ext = max(params.body_color_ext.w, 1e-3);
    let transmittance = pow(deep_linear, vec3<f32>(path_m * ext));

    // In-scattered light keeps the authored HUE but not its (near-black) brightness: a deep body
    // colour describes absorption, while the haze you actually see is sunlight scattered back out
    // of the volume. Normalising to the brightest channel preserves the authored tint and leaves
    // one honest magnitude constant instead of three invented ones.
    let peak = max(max(deep_linear.r, deep_linear.g), max(deep_linear.b, 1e-4));
    let haze = deep_linear / peak * INSCATTER_RADIANCE;

    // World-anchored caustics. The old pair of sines ran in SCREEN space, so the pattern swam
    // across the scene whenever the camera turned — it read as a lens artefact rather than light
    // on a surface. Keyed to the unprojected world position it stays put on the seabed.
    let caustic_xz = world_rel.xz;
    let caustic = 1.0 + CAUSTIC_STRENGTH *
        sin(caustic_xz.x * 1.9 + params.time * 1.7) *
        sin(caustic_xz.y * 1.6 - params.time * 1.3) *
        transmittance.g;

    return vec4<f32>(color * transmittance * caustic + haze * (vec3<f32>(1.0) - transmittance), 1.0);
}
