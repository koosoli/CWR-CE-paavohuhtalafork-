const MAX_EVENTS: u32 = 48u;
const BULLET: f32 = 0.0;
const OBJECT: f32 = 1.0;
const PLAYER: f32 = 2.0;
const EXPLOSION: f32 = 3.0;
const FOOTSTEP: f32 = 4.0;
const CONTINUOUS: f32 = 5.0;

struct InteractionEvent { position_radius: vec4<f32>, velocity_kind: vec4<f32>, time_life_foam_mass: vec4<f32>, direction_depth_flags: vec4<f32>, };
struct InteractionParams { domain: vec4<f32>, previous_domain: vec4<f32>, grid: vec4<f32>, physics: vec4<f32>, misc: vec4<f32>, weather: vec4<f32>, };
@group(0) @binding(0) var<uniform> params: InteractionParams;
@group(0) @binding(1) var<storage, read> events: array<InteractionEvent, 48>;
@group(0) @binding(2) var previous_field: texture_2d<f32>;
@group(0) @binding(3) var field_sampler: sampler;
@group(0) @binding(4) var next_field: texture_storage_2d<rgba16float, write>;

fn sample_previous(uv: vec2<f32>) -> vec4<f32> {
    let inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    return textureSampleLevel(previous_field, field_sampler, clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0) * inside;
}
fn hash2(p: vec2<f32>) -> f32 { return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453); }
fn type_mask(value: f32, expected: f32) -> f32 { return 1.0 - step(0.45, abs(value - expected)); }

@compute @workgroup_size(8, 8, 1)
fn interaction_update(@builtin(global_invocation_id) id: vec3<u32>) {
    let dimensions = textureDimensions(next_field);
    if (any(id.xy >= dimensions)) { return; }
    if (params.grid.w > 0.5) { textureStore(next_field, vec2<i32>(id.xy), vec4<f32>(0.0)); return; }
    let size = vec2<f32>(dimensions);
    let texel = 1.0 / size;
    let uv = (vec2<f32>(id.xy) + 0.5) / size;
    let world = params.domain.xy + uv * params.domain.z;
    let back_uv = (world - params.previous_domain.xy) * params.previous_domain.w;
    let centre = sample_previous(back_uv);
    let h_l = sample_previous(back_uv - vec2<f32>(texel.x, 0.0)).r;
    let h_r = sample_previous(back_uv + vec2<f32>(texel.x, 0.0)).r;
    let h_d = sample_previous(back_uv - vec2<f32>(0.0, texel.y)).r;
    let h_u = sample_previous(back_uv + vec2<f32>(0.0, texel.y)).r;
    let dt = clamp(params.grid.y, 0.0, 0.033);
    let calmness = clamp(params.weather.y, 0.0, 1.0);
    let cell = params.domain.z / max(size.x, 1.0);
    let laplacian = (h_l + h_r + h_d + h_u - 4.0 * centre.r) / max(cell * cell, 0.001);
    var velocity = centre.g + laplacian * params.physics.x * params.physics.x * dt * calmness;
    velocity = velocity * exp(-(params.physics.y + (1.0 - calmness) * 0.72) * dt);
    var height = centre.r + velocity * dt;
    var foam = centre.b * exp(-params.physics.w * dt);
    let event_count = min(u32(params.grid.z + 0.5), MAX_EVENTS);
    for (var index: u32 = 0u; index < event_count; index = index + 1u) {
        let event = events[index];
        let flags = u32(max(event.direction_depth_flags.w, 0.0) + 0.5);
        if ((flags & 1u) == 0u) { continue; }
        let delta = world - event.position_radius.xy;
        let radius = max(event.position_radius.z, cell * 1.25);
        // WTR-062 — Bounded event dispatch optimization:
        // Skip texel calculations for events whose radius box does not overlap the current position.
        if (abs(delta.x) > radius * 2.5 || abs(delta.y) > radius * 2.5) { continue; }
        let direction = normalize(event.direction_depth_flags.xy + event.velocity_kind.xy + vec2<f32>(0.001, 0.0));
        let capsule = (flags & (1u << 8u)) != 0u;
        let along = clamp(dot(delta, direction), -radius, radius);
        let capsule_distance = length(delta - direction * along);
        let distance = select(length(delta), capsule_distance, capsule);
        let q = distance / radius;
        if (q > 2.45) { continue; }
        let core = exp(-q * q * 5.0);
        let ring = exp(-((q - 0.88) * (q - 0.88)) * 20.0);
        let kind = event.velocity_kind.w;
        let bullet = type_mask(kind, BULLET);
        let object = type_mask(kind, OBJECT);
        let player = type_mask(kind, PLAYER);
        let explosion = type_mask(kind, EXPLOSION);
        let footstep = type_mask(kind, FOOTSTEP);
        let continuous = type_mask(kind, CONTINUOUS);
        let entry = clamp(abs(event.velocity_kind.z) / 8.0, 0.22, 2.0);
        let pulse = bullet * (-core * 1.45 + ring * 0.82) + object * (-core * 1.65 + ring * 2.05) + player * (-core * 1.10 + ring * 1.38) + explosion * (-core * 3.8 + ring * 5.3) + footstep * (-core * 0.26 + ring * 0.34) + continuous * (core * 0.16 + ring * 0.12);
        velocity = velocity + pulse * event.position_radius.w * entry * mix(0.075, 1.0, calmness * calmness);
        foam = max(foam, clamp((core + ring) * event.time_life_foam_mass.z * event.position_radius.w * 0.2, 0.0, 1.0));
    }
    if (params.weather.x > 0.0005 && calmness > 0.025) {
        let rain = hash2(floor(world * 0.8) + floor(params.misc.y));
        velocity = velocity + select(0.0, (rain - 0.5) * params.weather.x * 0.02, rain > 0.96);
    }
    let edge = min(min(uv.x, uv.y), min(1.0 - uv.x, 1.0 - uv.y));
    let edge_damp = smoothstep(0.0, 0.045, edge);
    height = clamp(height * edge_damp, -params.physics.z, params.physics.z);
    textureStore(next_field, vec2<i32>(id.xy), vec4<f32>(height, velocity * edge_damp, foam, 0.0));
}
