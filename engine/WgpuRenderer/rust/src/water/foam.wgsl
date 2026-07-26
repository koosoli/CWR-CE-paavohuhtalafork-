#import water_fft_sampling::fft_aperiodic_uv

struct InteractionParams { domain: vec4<f32>, previous_domain: vec4<f32>, grid: vec4<f32>, physics: vec4<f32>, misc: vec4<f32>, weather: vec4<f32>, };
struct WaterParams { world_origin: vec2<f32>, terrain_grid: f32, sea_level: f32, hm_width: u32, hm_height: u32, time: f32, wave_amp: f32, wave_choppy: f32, wave_speed: f32, wave_scale: f32, fade_start: f32, fade_end: f32, warp_amp: f32, spec_power: f32, spec_intensity: f32, alpha: f32, shadow_dim: f32, color_ext: f32, coast_fade: f32, shallow_color: vec4<f32>, deep_color: vec4<f32>, foam_width: f32, foam_intensity: f32, swash_amp: f32, swash_speed: f32, fft_control: vec4<f32>, fft_wind_sea: vec4<f32>, fft_cascade_lengths: vec4<f32>, flow_direction_speed: vec4<f32>, };
@group(0) @binding(0) var<uniform> interaction_params: InteractionParams;
@group(0) @binding(1) var<uniform> water: WaterParams;
@group(0) @binding(2) var previous_foam: texture_2d<f32>;
@group(0) @binding(3) var fft_displacement: texture_2d_array<f32>;
@group(0) @binding(4) var fft_auxiliary: texture_2d_array<f32>;
@group(0) @binding(5) var interaction_field: texture_2d<f32>;
@group(0) @binding(6) var field_sampler: sampler;
@group(0) @binding(7) var next_foam: texture_storage_2d<rgba16float, write>;
struct CascadeConfig {
    enabled: u32, resolution: u32, tile_length_x: f32, tile_length_y: f32,
    displacement_scale: f32, horiz_displacement_scale: f32, normal_scale: f32, foam_scale: f32,
    wind_speed: f32, wind_direction_rad: f32, fetch_meters: f32, water_depth_meters: f32,
    swell: f32, directional_spread: f32, short_wave_detail: f32, whitecap_threshold: f32,
    spectrum_seed: u32, phase_offset_seconds: f32, update_rate_hz: f32, pad: f32,
};
struct CascadeConfigs { config: array<CascadeConfig, 4>, };
@group(0) @binding(8) var<uniform> cascades: CascadeConfigs;

fn sample_history(uv: vec2<f32>) -> vec4<f32> {
    let inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    return textureSampleLevel(previous_foam, field_sampler, clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0) * inside;
}
fn fft_source(world: vec2<f32>) -> f32 {
    if (water.fft_control.x <= 0.5) { return 0.0; }
    var source = 0.0;
    for (var layer: i32 = 0; layer < 4; layer = layer + 1) {
        let cfg = cascades.config[u32(layer)];
        if (cfg.enabled == 0u) { continue; }
        let scale = max(water.fft_cascade_lengths[layer], 1.0);
        let uv = fft_aperiodic_uv(world, scale, layer, water.warp_amp);
        let auxiliary = textureSampleLevel(fft_auxiliary, field_sampler, uv, layer, 0.0);
        // Exact fft_unpack.glsl whitecap source:
        //   foam_factor = -min(0, jacobian - whitecap)
        //   grow_rate = dt * foam_amount * 7.5
        // Returning a per-second injection rate lets the history pass preserve the
        // reference threshold instead of whitening every mildly compressed texel.
        let foam_factor = max(cfg.whitecap_threshold - auxiliary.x, 0.0);
        source = max(source, foam_factor * max(cfg.foam_scale, 0.0) * 7.5);
    }
    return source;
}

@compute @workgroup_size(8, 8, 1)
fn foam_update(@builtin(global_invocation_id) id: vec3<u32>) {
    let dimensions = textureDimensions(next_foam);
    if (any(id.xy >= dimensions)) { return; }
    if (interaction_params.grid.w > 0.5) { textureStore(next_foam, vec2<i32>(id.xy), vec4<f32>(0.0)); return; }
    let size = vec2<f32>(dimensions);
    let uv = (vec2<f32>(id.xy) + 0.5) / size;
    let world = interaction_params.domain.xy + uv * interaction_params.domain.z;
    let dt = clamp(interaction_params.grid.y, 0.0, 0.033);
    let texel = vec2<f32>(1.0) / size;

    // Sample current interaction velocity field and central differences for WTR-082 divergence & vorticity
    let interaction_uv = (world - interaction_params.domain.xy) * interaction_params.domain.w;
    let centre_inter = textureSampleLevel(interaction_field, field_sampler, clamp(interaction_uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);
    let inter_l = textureSampleLevel(interaction_field, field_sampler, clamp(interaction_uv - vec2<f32>(texel.x, 0.0), vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);
    let inter_r = textureSampleLevel(interaction_field, field_sampler, clamp(interaction_uv + vec2<f32>(texel.x, 0.0), vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);
    let inter_d = textureSampleLevel(interaction_field, field_sampler, clamp(interaction_uv - vec2<f32>(0.0, texel.y), vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);
    let inter_u = textureSampleLevel(interaction_field, field_sampler, clamp(interaction_uv + vec2<f32>(0.0, texel.y), vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);

    let cell_spacing = max(interaction_params.domain.z / size.x, 0.01);
    let dvx_dx = (inter_r.g - inter_l.g) / (2.0 * cell_spacing);
    let dvy_dy = (inter_u.g - inter_d.g) / (2.0 * cell_spacing);
    let divergence = dvx_dx + dvy_dy;
    let convergence = smoothstep(0.0, -0.6, divergence);
    let vorticity = abs(dvy_dy - dvx_dx);

    // WTR-085 — Composite physical foam advection velocity (interaction velocity + ambient current + wind drift)
    let flow_dir = normalize(water.flow_direction_speed.xy + vec2<f32>(1e-4, 0.0)) * max(water.flow_direction_speed.z, 0.0);
    let wind_dir = normalize(water.fft_wind_sea.xy + vec2<f32>(1e-4, 0.0));
    let wind_drift = wind_dir * (0.08 + max(water.fft_wind_sea.z, 0.0) * 0.010);
    let surface_velocity = vec2<f32>(centre_inter.g * 0.4) + flow_dir + wind_drift;

    let previous_world = world - surface_velocity * dt;
    let previous_uv = (previous_world - interaction_params.previous_domain.xy) * interaction_params.previous_domain.w;
    let history = sample_history(previous_uv);

    // WTR-081 — Physical breaking energy and interaction aeration injection
    let fft_breaker = fft_source(world);
    let aeration = clamp(centre_inter.b, 0.0, 1.0);
    let wake_breaker = clamp(convergence * 0.35 + centre_inter.g * 0.15 + aeration, 0.0, 1.0);

    // Dissipation and decay rates
    let breaker_decay = history.r * exp(-dt * 3.5);
    let wake_decay = history.g * exp(-dt * 2.8);
    let aeration_decay = history.b * exp(-dt * 4.2);

    let breaker_injection = 1.0 - exp(-fft_breaker * dt);
    let wake_injection = 1.0 - exp(-wake_breaker * dt * 1.50);

    let breaker_foam = 1.0 - (1.0 - breaker_decay) * (1.0 - breaker_injection);
    let wake_foam = 1.0 - (1.0 - wake_decay) * (1.0 - wake_injection);
    let air_entrainment = max(aeration_decay, aeration);

    let edge = min(min(uv.x, uv.y), min(1.0 - uv.x, 1.0 - uv.y));
    let edge_mask = smoothstep(0.002, 0.018, edge);

    textureStore(next_foam, vec2<i32>(id.xy), vec4<f32>(
        breaker_foam * edge_mask,
        wake_foam * edge_mask,
        air_entrainment * edge_mask,
        clamp(vorticity, 0.0, 1.0) * edge_mask
    ));
}
