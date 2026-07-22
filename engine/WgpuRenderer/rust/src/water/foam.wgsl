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

fn sample_history(uv: vec2<f32>) -> vec4<f32> {
    let inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    return textureSampleLevel(previous_foam, field_sampler, clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0) * inside;
}
fn fft_source(world: vec2<f32>) -> f32 {
    if (water.fft_control.x <= 0.5) { return 0.0; }
    var source = 0.0;
    for (var layer: i32 = 0; layer < 4; layer = layer + 1) {
        let scale = max(water.fft_cascade_lengths[layer], 1.0);
        let uv = fract(world / scale);
        let displacement = textureSampleLevel(fft_displacement, field_sampler, uv, layer, 0.0);
        let auxiliary = textureSampleLevel(fft_auxiliary, field_sampler, uv, layer, 0.0);
        // auxiliary = (J, max(1-J, 0), positive curvature, slope magnitude squared).
        // Require geometric convergence and a steep crest together: this excludes broad
        // low-curvature compression from becoming a persistent coastal carpet.
        let crest = displacement.w;
        let compression = auxiliary.y;
        let curvature = auxiliary.z;
        let steepness = sqrt(max(auxiliary.w, 0.0));
        // The C++ calm sea is deliberately only 0.08, so these gates begin above
        // numerical texture noise but still admit its occasional focused crest.
        let breaker = smoothstep(0.008, 0.026, compression)
            * smoothstep(0.025, 0.090, crest)
            * smoothstep(0.020, 0.090, steepness)
            * smoothstep(0.0015, 0.012, curvature);
        source = max(source, breaker);
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
    let wind = normalize(water.fft_wind_sea.xy + vec2<f32>(0.0001, 0.0));
    let drift = wind * (0.10 + max(water.fft_wind_sea.z, 0.0) * 0.012);
    let previous_world = world - drift * dt;
    let previous_uv = (previous_world - interaction_params.previous_domain.xy) * interaction_params.previous_domain.w;
    let history = sample_history(previous_uv);
    let interaction_uv = (world - interaction_params.domain.xy) * interaction_params.domain.w;
    let interaction = textureSampleLevel(interaction_field, field_sampler, clamp(interaction_uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);
    let fft = fft_source(world);
    let aeration = clamp(interaction.b, 0.0, 1.0);
    let source = max(fft, aeration);
    // Keep breakers visible briefly, but do not let repeated shallow/coastal sources
    // turn into a permanent white band.
    let decayed = history.r * exp(-dt * 5.5);
    let injection = 1.0 - exp(-source * dt * 0.85);
    let coverage = 1.0 - (1.0 - decayed) * (1.0 - injection);
    let age = mix(min(history.g + dt * 0.08, 1.0), 0.0, clamp(injection * 2.0, 0.0, 1.0));
    let stored_aeration = max(history.b * exp(-dt * 4.8), aeration);
    let edge = min(min(uv.x, uv.y), min(1.0 - uv.x, 1.0 - uv.y));
    textureStore(next_foam, vec2<i32>(id.xy), vec4<f32>(coverage * smoothstep(0.002, 0.018, edge), age, stored_aeration, 0.0));
}
