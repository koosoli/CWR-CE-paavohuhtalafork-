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
    let color = textureSampleLevel(scene_tex, scene_samp, in.uv, 0.0).rgb;
    let pixel = vec2<i32>(in.clip.xy);
    // Reversed-Z depth is proportional to 1/distance. The approximation deliberately
    // saturates for sky/cleared depth, producing dense distant underwater haze.
    let depth = textureLoad(scene_depth, pixel, 0);
    let path_m = clamp(0.12 / max(depth, 0.002), 0.5, 60.0);
    let transmittance = exp(-vec3<f32>(0.075, 0.035, 0.015) * path_m);
    let haze = vec3<f32>(0.018, 0.115, 0.145);
    let caustic = 1.0 + 0.035 * sin(in.uv.x * 90.0 + params.time * 1.7) * sin(in.uv.y * 74.0 - params.time * 1.3);
    let fog = 1.0 - exp(-path_m * 0.045);
    return vec4<f32>(mix(color * transmittance * caustic, haze, fog), 1.0);
}
