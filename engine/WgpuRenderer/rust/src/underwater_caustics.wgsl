struct Params {
    time_height_range_ext: vec4<f32>,
    camera_pos_layers: vec4<f32>,
    sun_dir_debug: vec4<f32>,
    sun_radiance: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
    shallow_color: vec4<f32>,
    deep_color: vec4<f32>,
    cascade_lengths: vec4<f32>,
    water_controls: vec4<f32>,
    underwater_tuning: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var fft_dynamics: texture_2d_array<f32>;
@group(0) @binding(2) var fft_auxiliary: texture_2d_array<f32>;
@group(0) @binding(3) var fft_samp: sampler;
@group(0) @binding(4) var caustic_out: texture_storage_2d<rgba16float, write>;

fn fft_hash(cell: vec2<i32>) -> f32 {
    let c = vec2<u32>(cell) & vec2<u32>(0xffffu);
    var n = c.x * 1597334677u + c.y * 3812015801u;
    n = (n ^ (n >> 15u)) * 2246822519u;
    n = n ^ (n >> 13u);
    return f32(n & 0x00ffffffu) / 16777216.0;
}

fn fft_value_noise(p: vec2<f32>) -> f32 {
    let cell = vec2<i32>(floor(p));
    let f = fract(p);
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = fft_hash(cell);
    let b = fft_hash(cell + vec2<i32>(1, 0));
    let c = fft_hash(cell + vec2<i32>(0, 1));
    let d = fft_hash(cell + vec2<i32>(1, 1));
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

fn fft_aperiodic_uv(world_xz: vec2<f32>, tile_length: f32, layer: i32, amplitude: f32) -> vec2<f32> {
    if (amplitude <= 0.0) {
        return fract(world_xz / max(tile_length, 1.0));
    }
    let l = f32(layer);
    let broad_p = world_xz * 0.00173 + vec2<f32>(l * 13.7, l * -19.1);
    let fine_p = world_xz * 0.00891 + vec2<f32>(l * -7.3, l * 11.9);
    let broad = vec2<f32>(
        fft_value_noise(broad_p),
        fft_value_noise(broad_p + vec2<f32>(41.3, 17.9))
    ) * 2.0 - vec2<f32>(1.0);
    let fine = vec2<f32>(
        fft_value_noise(fine_p),
        fft_value_noise(fine_p + vec2<f32>(23.1, 37.7))
    ) * 2.0 - vec2<f32>(1.0);
    let warped = world_xz + (broad * 0.72 + fine * 0.28) * max(amplitude, 0.0) *
        (1.0 + l * 0.17);
    let angle = 0.173 * l + 0.071;
    let ca = cos(angle);
    let sa = sin(angle);
    let rotated = vec2<f32>(
        ca * warped.x - sa * warped.y,
        sa * warped.x + ca * warped.y
    );
    return fract(rotated / max(tile_length, 1.0) + vec2<f32>(0.173 * l, 0.347 * l));
}

@compute @workgroup_size(8, 8, 1)
fn cs_underwater_caustics(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(caustic_out);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    // The field covers the compositor's full 120 m ray range around the camera.
    let world_xz = params.camera_pos_layers.xz + (uv - 0.5) * 256.0;
    let active_layers = clamp(i32(params.camera_pos_layers.w), 0, 4);
    let warp = params.water_controls.x;
    var energy = 0.0;
    var weight_sum = 0.0;
    for (var layer = 0; layer < 4; layer = layer + 1) {
        if (layer >= active_layers) {
            break;
        }
        let length_m = max(params.cascade_lengths[layer], 1.0);
        let fft_uv = fft_aperiodic_uv(world_xz, length_m, layer, warp);
        let dynamics = textureSampleLevel(fft_dynamics, fft_samp, fft_uv, layer, 0.0);
        let auxiliary = textureSampleLevel(fft_auxiliary, fft_samp, fft_uv, layer, 0.0);
        let compression = clamp(auxiliary.y, 0.0, 1.5);
        let curvature = 1.0 - exp(-max(auxiliary.z, 0.0) * 0.22);
        let slope_focus = 1.0 - exp(-length(dynamics.xy) * 0.65);
        let cascade_weight = 1.0 / (1.0 + f32(layer) * 0.45);
        energy = energy + (compression * 0.58 + curvature * 0.30 +
            slope_focus * 0.12) * cascade_weight;
        weight_sum = weight_sum + cascade_weight;
    }
    let focused = clamp(energy / max(weight_sum, 1e-3), 0.0, 1.0);
    let caustic = smoothstep(0.08, 0.62, focused);
    textureStore(caustic_out, vec2<i32>(gid.xy), vec4<f32>(caustic, focused, 0.0, 1.0));
}
