struct ShadowPass { light_vp: mat4x4<f32>, cam_pos: vec4<f32> };
struct TerrainParams {
    world_origin: vec2<f32>, land_grid: f32, terrain_grid: f32,
    hm_width: u32, hm_height: u32, land_range: u32, data_scale: f32,
    sea_level: f32, time: f32, swash_speed: f32, swash_amp: f32,
    wet_height: f32, wet_darken: f32, pad_a: f32, pad_b: f32,
};
struct GrassTrack { x: f32, z: f32, radius: f32, age: f32 };
struct GrassDownwash { x: f32, z: f32, radius: f32, strength: f32 };
struct GrassParams {
    density: f32, spacing: f32, near_radius: f32, enabled: f32,
    blade_height: f32, wind_strength: f32, wind_direction: f32, far_radius: f32,
    interactor_x: f32, interactor_z: f32, interactor_radius: f32, interactor_strength: f32,
    tracks: array<GrassTrack, 96>, downwash: array<GrassDownwash, 4>, debug_flags: vec4<f32>, render_flags: vec4<f32>,
    species_mix: vec4<f32>,
    // Layout must mirror grass.wgsl exactly -- same uniform buffer.
    look: vec4<f32>,
    shape_mix: vec4<f32>,
    cards: vec4<f32>,
};
// Must mirror grass.wgsl's 32-byte layout exactly: both shaders bind the same
// instance buffer through the shared `data_layout`.
struct GrassInstance {
    pos_seed: vec4<f32>,
    packed: vec4<u32>,
};

@group(0) @binding(0) var<uniform> shadow: ShadowPass;
@group(1) @binding(0) var<uniform> terrain: TerrainParams;
@group(1) @binding(1) var heightmap: texture_2d<f32>;
@group(1) @binding(2) var geography: texture_2d<u32>;
@group(2) @binding(0) var<uniform> grass: GrassParams;
@group(2) @binding(1) var<storage, read_write> instances: array<GrassInstance>;
@group(2) @binding(2) var<storage, read_write> placement_count: array<atomic<u32>>;

fn hash11(p: vec2<f32>) -> f32 { return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453123); }
fn hash_u32(x_in: u32) -> u32 {
    var x = x_in;
    x = (x ^ (x >> 16u)) * 0x7feb352du;
    x = (x ^ (x >> 15u)) * 0x846ca68bu;
    return x ^ (x >> 16u);
}
fn hash_cell01(cell: vec2<i32>, salt: u32) -> f32 {
    let x = bitcast<u32>(cell.x);
    let z = bitcast<u32>(cell.y);
    let h = hash_u32(x * 0x9e3779b9u ^ z * 0x85ebca6bu ^ salt);
    return f32(h >> 8u) * (1.0 / 16777216.0);
}
fn clump_noise(world_xz: vec2<f32>, frequency: f32, salt: u32) -> f32 {
    let p = world_xz * frequency;
    let base = vec2<i32>(floor(p));
    let f = fract(p);
    let s = f * f * (vec2<f32>(3.0) - 2.0 * f);
    let a = hash_cell01(base, salt);
    let b = hash_cell01(base + vec2<i32>(1, 0), salt);
    let c = hash_cell01(base + vec2<i32>(0, 1), salt);
    let d = hash_cell01(base + vec2<i32>(1, 1), salt);
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

// Mirrors grass.wgsl's world-space wind field. Shadow cascades now use the
// same moving gusts as the visible blades, instead of the former sine wave.
fn sample_wind_field(world_xz: vec2<f32>, height_t: f32, seed: f32) -> vec4<f32> {
    let strength = clamp(grass.wind_strength, 0.0, 3.0);
    let base_angle = grass.wind_direction * 0.01745329252;
    let base_direction = vec2<f32>(cos(base_angle), sin(base_angle));
    // Negated scroll: see the convention note in grass.wgsl. Both copies must
    // agree or shadows drift against the blades that cast them.
    let broad_scroll = -base_direction * terrain.time * (16.0 + strength * 9.0);
    let gust_scroll = -base_direction * terrain.time * (30.0 + strength * 15.0);
    let direction_noise = clump_noise(world_xz + broad_scroll, 0.006, 0x0d7e31a5u);
    let gust_noise = clump_noise(world_xz + gust_scroll, 0.022, 0xa12f7c59u);
    let gust = mix(0.18, 1.0, pow(smoothstep(0.40, 0.84, gust_noise), 2.0));
    let direction_angle = base_angle + (direction_noise - 0.5) * min(strength, 1.5) * 1.10;
    let flutter_scroll = gust_scroll * 2.1 + vec2<f32>(terrain.time * 3.7, -terrain.time * 2.9);
    let flutter_noise = clump_noise(world_xz + flutter_scroll + base_direction * (height_t * height_t * 4.0) +
                                    vec2<f32>(seed * 19.0, seed * 31.0), 0.105, 0x3f5a91c7u);
    let turbulence = (flutter_noise - 0.5) * (0.035 + 0.085 * gust) * height_t;
    return vec4<f32>(cos(direction_angle), sin(direction_angle), gust, turbulence);
}

@vertex
fn vs_grass_shadow(@builtin(vertex_index) vertex_index: u32, @builtin(instance_index) instance_index: u32) -> @builtin(position) vec4<f32> {
    let inst = instances[instance_index].pos_seed;
    let seed = inst.w;
    let field = clump_noise(inst.xz, 0.075, 0x48ac2f19u);
    let angle = mix(seed * 6.2831853, field * 6.2831853, grass.debug_flags.y);
    let side = vec3<f32>(cos(angle), 0.0, -sin(angle));
    let forward = vec3<f32>(sin(angle), 0.0, cos(angle));
    let height_seed = mix(hash11(inst.xz + 2.0), clump_noise(inst.xz, 0.21, 0xa47f3cd1u), grass.debug_flags.y * 0.72);
    // Species shape must mirror grass.wgsl's species_shape_mixed(): a broad weed
    // leaf casts a broad shadow, and a flower stem must not taper to a point.
    // This file re-implements it rather than importing, so the two drift unless
    // edited together -- which is why the numbers below are the same literals.
    let species = instances[instance_index].packed.w & 7u;
    var legacy = vec3<f32>(1.0, 1.0, 0.65);
    if (species >= 6u) { legacy = vec3<f32>(0.85, 1.15, 0.18); }
    else if (species >= 4u) { legacy = vec3<f32>(1.9, 0.82, 0.42); }
    var varied = vec3<f32>(1.0, 1.0, 0.65);
    switch (species) {
        case 0u: { varied = vec3<f32>(0.74, 1.06, 0.34); }
        case 1u: { varied = vec3<f32>(1.00, 1.00, 0.26); }
        case 2u: { varied = vec3<f32>(1.34, 0.92, 0.20); }
        case 3u: { varied = vec3<f32>(0.62, 1.18, 0.46); }
        case 4u: { varied = vec3<f32>(1.90, 0.82, 0.22); }
        case 5u: { varied = vec3<f32>(1.52, 0.70, 0.15); }
        case 6u: { varied = vec3<f32>(0.85, 1.15, 0.12); }
        default: { varied = vec3<f32>(0.68, 1.32, 0.09); }
    }
    let shape = mix(legacy, varied, clamp(grass.shape_mix.x, 0.0, 1.0));
    let height = mix(0.35, 1.05, height_seed) * grass.blade_height * shape.y;
    let bend_jitter = 1.0 + (hash11(inst.xz + 57.0) * 2.0 - 1.0) * clamp(grass.shape_mix.z, 0.0, 1.0);
    // Arch must mirror grass.wgsl exactly, height scaling included, or a blade's shadow is cast
    // by a straighter blade than the one being drawn.
    let arch = max(grass.cards.w, 0.0);
    let static_bend = forward * mix(0.10, 0.34, hash11(inst.xz + 31.0)) * bend_jitter * arch * height;
    // Flattening cached by cs_place. Without this the shadow pass rebuilt an
    // upright blade, so grass a player had walked flat kept casting a full
    // standing shadow.
    let inst_packed = instances[instance_index].packed;
    let crush_dir = unpack2x16snorm(inst_packed.x);
    let crush_data = unpack2x16unorm(inst_packed.y);
    let crush = crush_data.x;
    let cached_wash = crush_data.y;
    var rotor_wash = cached_wash;
    if (grass.interactor_strength > 1.001 && grass.interactor_radius > 0.01) {
        let delta = inst.xz - vec2<f32>(grass.interactor_x, grass.interactor_z);
        let live_wash = (1.0 - smoothstep(grass.interactor_radius * 0.18, grass.interactor_radius, length(delta))) * crush;
        rotor_wash = max(cached_wash, live_wash);
    }
    let crush_bend = vec3<f32>(crush_dir.x, 0.0, crush_dir.y) * height * (0.55 * crush);
    let crushed_height = height * (1.0 - 0.55 * crush);
    let card = vertex_index / 30u;
    let packed = vertex_index % 30u;
    let segment = packed / 6u;
    let corner = packed % 6u;
    let upper = corner == 2u || corner == 4u || corner == 5u;
    let left = corner == 0u || corner == 3u || corner == 5u;
    let t = f32(segment + select(0u, 1u, upper)) / 5.0;
    let axis = select(side, forward, card != 0u);
    let wind = sample_wind_field(inst.xz, t, seed);
    let wind_bend = vec3<f32>(wind.x, 0.0, wind.y) * grass.wind_strength *
        (0.035 + 0.21 * wind.z + wind.w);
    let crush_flutter = vec3<f32>(sin(terrain.time * 28.0 + seed * 37.0), 0.0,
                                  cos(terrain.time * 33.0 + seed * 53.0)) * height * (0.95 * rotor_wash);
    let bend = (static_bend + wind_bend) * (1.0 - 0.55 * crush) + crush_bend + crush_flutter;
    // species_mix.z mirrors the Grass-tab blade width multiplier, or a widened
    // blade would cast the shadow of a thin one.
    // Card widening and taper jitter mirror grass.wgsl: a shadow cast by the
    // un-widened blade would not match the silhouette actually drawn.
    let card_widen = mix(1.0, max(grass.cards.z, 1.0), grass.cards.x);
    let taper_jitter = 1.0 + (hash11(inst.xz + 71.0) * 2.0 - 1.0) * clamp(grass.shape_mix.y, 0.0, 1.0);
    let width = mix(0.018, 0.045, hash11(inst.xz + 9.0)) * shape.x *
        max(grass.species_mix.z, 0.05) * card_widen *
        pow(max(1.0 - t, 0.0), max(shape.z * taper_jitter, 0.05));
    let lateral = select(axis * width, -axis * width, left);
    let world = inst.xyz + lateral + vec3<f32>(0.0, crushed_height * t, 0.0) + bend * (t * t);
    return shadow.light_vp * vec4<f32>(world - shadow.cam_pos.xyz, 1.0);
}
