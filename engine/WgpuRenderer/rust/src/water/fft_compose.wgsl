struct WaterParams { world_origin: vec2<f32>, terrain_grid: f32, sea_level: f32, hm_width: u32, hm_height: u32, time: f32, wave_amp: f32, wave_choppy: f32, wave_speed: f32, wave_scale: f32, fade_start: f32, fade_end: f32, warp_amp: f32, spec_power: f32, spec_intensity: f32, alpha: f32, shadow_dim: f32, color_ext: f32, coast_fade: f32, shallow_color: vec4<f32>, deep_color: vec4<f32>, foam_width: f32, foam_intensity: f32, swash_amp: f32, swash_speed: f32, fft_control: vec4<f32>, fft_wind_sea: vec4<f32>, fft_cascade_lengths: vec4<f32>, flow_direction_speed: vec4<f32> };
@group(0) @binding(0) var<uniform> water: WaterParams;
@group(0) @binding(1) var pack0: texture_2d_array<f32>;
@group(0) @binding(2) var pack1: texture_2d_array<f32>;
@group(0) @binding(3) var pack2: texture_2d_array<f32>;
@group(0) @binding(4) var displacement: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(5) var dynamics: texture_storage_2d_array<rgba16float, write>;
@group(0) @binding(6) var auxiliary: texture_storage_2d_array<rgba16float, write>;
fn wrap(v: i32, n: i32) -> i32 { return (v % n + n) % n; }
@compute @workgroup_size(8, 8, 1)
fn fft_compose(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(displacement);
    if (id.x >= dims.x || id.y >= dims.y || id.z >= 4u) { return; }
    let c = vec2<i32>(id.xy);
    let layer = i32(id.z);
    let p0 = textureLoad(pack0, c, layer, 0);
    let p1 = textureLoad(pack1, c, layer, 0);
    let p2 = textureLoad(pack2, c, layer, 0);
    let cell = water.fft_cascade_lengths[id.z] / f32(dims.x);
    let h_l = textureLoad(pack0, vec2<i32>(wrap(c.x - 1, i32(dims.x)), c.y), layer, 0).x;
    let h_r = textureLoad(pack0, vec2<i32>(wrap(c.x + 1, i32(dims.x)), c.y), layer, 0).x;
    let h_d = textureLoad(pack0, vec2<i32>(c.x, wrap(c.y - 1, i32(dims.y))), layer, 0).x;
    let h_u = textureLoad(pack0, vec2<i32>(c.x, wrap(c.y + 1, i32(dims.y))), layer, 0).x;
    // Preserve dynamics.xy as world-space height slopes for fft_normal. They are
    // reconstructed from the inverse-transformed height so the spare spectral lanes
    // can carry the complete horizontal-displacement Jacobian.
    let slope_x = (h_r - h_l) / max(2.0 * cell, 0.001);
    let slope_z = (h_u - h_d) / max(2.0 * cell, 0.001);
    let curvature = -(h_l + h_r + h_d + h_u - 4.0 * p0.x) / max(cell * cell, 0.001);
    let crest = clamp(max(p0.x, 0.0) + max(curvature, 0.0) * 0.1, 0.0, 1.0);
    let d_dxdx = p0.z;
    let d_dxdz = p1.x;
    let d_dzdx = p1.x;
    let d_dzdz = p1.z;
    let jacobian = (1.0 + d_dxdx) * (1.0 + d_dzdz) - d_dxdz * d_dzdx;
    let compression = max(1.0 - jacobian, 0.0);
    let slope_variance = slope_x * slope_x + slope_z * slope_z;
    // displacement = (Dx, height, Dz, crest); dynamics.xy = height slope.
    // auxiliary = (signed horizontal Jacobian J, compression max(1-J,0),
    //              positive crest curvature, local height-slope magnitude squared).
    textureStore(displacement, c, layer, vec4<f32>(p2.x, p0.x, p2.z, crest));
    textureStore(dynamics, c, layer, vec4<f32>(slope_x, slope_z, 0.0, 0.0));
    textureStore(auxiliary, c, layer, vec4<f32>(jacobian, compression, max(curvature, 0.0), slope_variance));
}
