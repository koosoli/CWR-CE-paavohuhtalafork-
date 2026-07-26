struct ShadowPass { light_vp: mat4x4<f32>, cam_pos: vec4<f32> };
struct TerrainParams {
    world_origin: vec2<f32>, land_grid: f32, terrain_grid: f32,
    hm_width: u32, hm_height: u32, land_range: u32, data_scale: f32,
    sea_level: f32, time: f32, swash_speed: f32, swash_amp: f32,
    wet_height: f32, wet_darken: f32, pad_a: f32, pad_b: f32,
};
struct GrassTrack { x: f32, z: f32, radius: f32, age: f32 };
struct GrassParams {
    density: f32, spacing: f32, near_radius: f32, enabled: f32,
    blade_height: f32, wind_strength: f32, wind_direction: f32, far_radius: f32,
    interactor_x: f32, interactor_z: f32, interactor_radius: f32, interactor_strength: f32,
    tracks: array<GrassTrack, 96>, debug_flags: vec4<f32>,
};
struct GrassInstance { pos_seed: vec4<f32> };

@group(0) @binding(0) var<uniform> shadow: ShadowPass;
@group(1) @binding(0) var<uniform> terrain: TerrainParams;
@group(1) @binding(1) var heightmap: texture_2d<f32>;
@group(1) @binding(2) var geography: texture_2d<u32>;
@group(2) @binding(0) var<uniform> grass: GrassParams;
@group(2) @binding(1) var<storage, read_write> instances: array<GrassInstance>;
@group(2) @binding(2) var<storage, read_write> placement_count: array<atomic<u32>>;

fn hash11(p: vec2<f32>) -> f32 { return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453123); }

@vertex
fn vs_grass_shadow(@builtin(vertex_index) vertex_index: u32, @builtin(instance_index) instance_index: u32) -> @builtin(position) vec4<f32> {
    let inst = instances[instance_index].pos_seed;
    let seed = inst.w;
    let angle = seed * 6.2831853;
    let side = vec3<f32>(cos(angle), 0.0, -sin(angle));
    let forward = vec3<f32>(sin(angle), 0.0, cos(angle));
    let height = mix(0.35, 1.05, hash11(inst.xz + 2.0)) * grass.blade_height;
    let wind_angle = grass.wind_direction * 0.01745329252;
    let wind_dir = vec2<f32>(cos(wind_angle), sin(wind_angle));
    let gust = sin(dot(inst.xz, wind_dir * 0.065) + terrain.time * (0.85 + grass.wind_strength * 0.55) + seed * 6.28);
    let bend = forward * mix(0.055, 0.19, hash11(inst.xz + 31.0)) +
        vec3<f32>(wind_dir.x, 0.0, wind_dir.y) * grass.wind_strength * (0.035 + 0.14 * gust);
    let card = vertex_index / 30u;
    let packed = vertex_index % 30u;
    let segment = packed / 6u;
    let corner = packed % 6u;
    let upper = corner == 2u || corner == 4u || corner == 5u;
    let left = corner == 0u || corner == 3u || corner == 5u;
    let t = f32(segment + select(0u, 1u, upper)) / 5.0;
    let axis = select(side, forward, card != 0u);
    let width = mix(0.018, 0.045, hash11(inst.xz + 9.0)) * pow(max(1.0 - t, 0.0), 0.65);
    let lateral = select(axis * width, -axis * width, left);
    let world = inst.xyz + lateral + vec3<f32>(0.0, height * t, 0.0) + bend * (t * t);
    return shadow.light_vp * vec4<f32>(world - shadow.cam_pos.xyz, 1.0);
}
