struct InteractionParams { domain: vec4<f32>, previous_domain: vec4<f32>, grid: vec4<f32>, physics: vec4<f32>, misc: vec4<f32>, weather: vec4<f32>, };
struct WaterParams { world_origin: vec2<f32>, terrain_grid: f32, sea_level: f32, hm_width: u32, hm_height: u32, time: f32, wave_amp: f32, wave_choppy: f32, wave_speed: f32, wave_scale: f32, fade_start: f32, fade_end: f32, warp_amp: f32, spec_power: f32, spec_intensity: f32, alpha: f32, shadow_dim: f32, color_ext: f32, coast_fade: f32, shallow_color: vec4<f32>, deep_color: vec4<f32>, foam_width: f32, foam_intensity: f32, swash_amp: f32, swash_speed: f32, fft_control: vec4<f32>, fft_wind_sea: vec4<f32>, fft_cascade_lengths: vec4<f32>, flow_direction_speed: vec4<f32>, };

@group(0) @binding(0) var<uniform> interaction_params: InteractionParams;
@group(0) @binding(1) var<uniform> water: WaterParams;
@group(0) @binding(2) var interaction_field: texture_2d<f32>;
@group(0) @binding(3) var foam_history: texture_2d<f32>;
@group(0) @binding(4) var field_sampler: sampler;

struct WhitewaterParticle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    age_lifetime: vec2<f32>,
    radius_density: vec2<f32>,
    state_type_seed: vec4<u32>, // x: state (0=spray, 1=foam, 2=bubble), y: sourceType, z: seed, w: pad
};

struct ParticleBuffer {
    count: u32,
    pad: vec3<u32>,
    particles: array<WhitewaterParticle>,
};

@group(1) @binding(0) var<storage, read_write> particle_buf: ParticleBuffer;

fn hash13(p3: vec3<f32>) -> f32 {
    var p = fract(p3 * vec3<f32>(0.1031, 0.1030, 0.0973));
    p += dot(p, p.yzx + 33.33);
    return fract((p.x + p.y) * p.z);
}

@compute @workgroup_size(64, 1, 1)
fn whitewater_simulate(@builtin(global_invocation_id) id: vec3<u32>) {
    let particle_idx = id.x;
    if (particle_idx >= particle_buf.count) { return; }

    var p = particle_buf.particles[particle_idx];
    if (p.age_lifetime.x >= p.age_lifetime.y) { return; }

    let dt = clamp(interaction_params.grid.y, 0.0, 0.033);
    p.age_lifetime.x += dt;

    let world_xz = p.position.xz;
    let interaction_uv = (world_xz - interaction_params.domain.xy) * interaction_params.domain.w;
    let interaction = textureSampleLevel(interaction_field, field_sampler, clamp(interaction_uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0);
    let surface_h = water.sea_level + interaction.r;

    let wind_dir = normalize(water.fft_wind_sea.xy + vec2<f32>(1e-4, 0.0));
    let wind_vel = vec3<f32>(wind_dir.x, 0.0, wind_dir.y) * max(water.fft_wind_sea.z, 0.5);

    // State 0: Spray / Spindrift (above surface)
    if (p.state_type_seed.x == 0u) {
        p.velocity += (vec3<f32>(0.0, -9.81, 0.0) + (wind_vel - p.velocity) * 0.4) * dt;
        p.position += p.velocity * dt;
        if (p.position.y <= surface_h) {
            p.position.y = surface_h;
            p.state_type_seed.x = 1u; // Transition to surface foam
            p.velocity *= 0.3;
        }
    }
    // State 1: Surface Foam Clump (on interface)
    else if (p.state_type_seed.x == 1u) {
        let flow_dir = normalize(water.flow_direction_speed.xy + vec2<f32>(1e-4, 0.0)) * max(water.flow_direction_speed.z, 0.0);
        let surf_vel = vec3<f32>(interaction.g * 0.5 + flow_dir.x, 0.0, flow_dir.y);
        p.velocity = mix(p.velocity, surf_vel + wind_vel * 0.05, dt * 4.0);
        p.position += p.velocity * dt;
        p.position.y = surface_h;
    }
    // State 2: Underwater Bubble (below surface)
    else if (p.state_type_seed.x == 2u) {
        let buoyancy = vec3<f32>(0.0, 3.2, 0.0);
        p.velocity = mix(p.velocity, buoyancy, dt * 2.5);
        p.position += p.velocity * dt;
        if (p.position.y >= surface_h) {
            p.position.y = surface_h;
            p.state_type_seed.x = 1u; // Burst at surface into foam
        }
    }

    particle_buf.particles[particle_idx] = p;
}
