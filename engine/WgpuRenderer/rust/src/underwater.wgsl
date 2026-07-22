struct Params { time: f32, _pad: vec3<f32> };
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
    let path_m = clamp(0.12 / max(depth, 0.002), 0.5, 60.0);
    let transmittance = exp(-vec3<f32>(0.075, 0.035, 0.015) * path_m);
    let haze = vec3<f32>(0.018, 0.115, 0.145);
    let caustic = 1.0 + 0.035 * sin(in.uv.x * 90.0 + params.time * 1.7) * sin(in.uv.y * 74.0 - params.time * 1.3);
    let fog = 1.0 - exp(-path_m * 0.045);
    return vec4<f32>(mix(color * transmittance * caustic, haze, fog), 1.0);
}
