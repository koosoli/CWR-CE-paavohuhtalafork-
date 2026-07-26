#import frame::{frame, reverse_z, apply_fog, sky_irradiance, terrain_sun_shadow, sky_vis_ao}
#import gbuffer::oct_encode

struct TerrainParams {
    world_origin: vec2<f32>,
    land_grid: f32,
    terrain_grid: f32,
    hm_width: u32,
    hm_height: u32,
    land_range: u32,
    data_scale: f32,
    sea_level: f32,
    time: f32,
    swash_speed: f32,
    swash_amp: f32,
    wet_height: f32,
    wet_darken: f32,
    pad_a: f32,
    pad_b: f32,
};

struct GrassTrack {
    x: f32,
    z: f32,
    radius: f32,
    age: f32,
};

struct GrassParams {
    density: f32,
    spacing: f32,
    near_radius: f32,
    enabled: f32,
    blade_height: f32,
    wind_strength: f32,
    wind_direction: f32,
    far_radius: f32,
    interactor_x: f32,
    interactor_z: f32,
    interactor_radius: f32,
    interactor_strength: f32,
    tracks: array<GrassTrack, 96>,
    debug_flags: vec4<f32>,
};

struct GrassInstance { pos_seed: vec4<f32> };

@group(1) @binding(0) var<uniform> terrain: TerrainParams;
@group(1) @binding(1) var heightmap: texture_2d<f32>;
@group(1) @binding(2) var geography: texture_2d<u32>;
@group(2) @binding(0) var<uniform> grass: GrassParams;
@group(2) @binding(1) var<storage, read_write> instances: array<GrassInstance>;
@group(2) @binding(2) var<storage, read_write> placement_count: array<atomic<u32>>;

const GRID_DIM: u32 = 512u;
const MAX_INSTANCES: u32 = 262144u;
const FAR_GRID_DIM: u32 = 384u;
const MAX_FAR_INSTANCES: u32 = 147456u;
const MID_GRID_DIM: u32 = 384u;
const MAX_MID_INSTANCES: u32 = 147456u;
const BLADE_SEGMENTS: u32 = 5u;
const VERTS_PER_CARD: u32 = BLADE_SEGMENTS * 6u;

fn hash11(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

// Stable integer-cell random numbers. The old sine hash plus a half-cell
// jitter left the placement grid faintly visible on broad terrain. This
// avalanche hash and deliberately overlapping jitter remove the rows while
// retaining deterministic world-space placement as the camera moves.
fn hash_u32(x_in: u32) -> u32 {
    var x = x_in;
    x = (x ^ (x >> 16u)) * 0x7feb352du;
    x = (x ^ (x >> 15u)) * 0x846ca68bu;
    return x ^ (x >> 16u);
}

fn hash_cell(cell: vec2<i32>, salt: u32) -> u32 {
    let x = bitcast<u32>(cell.x);
    let z = bitcast<u32>(cell.y);
    return hash_u32(x * 0x9e3779b9u ^ z * 0x85ebca6bu ^ salt);
}

fn hash_cell01(cell: vec2<i32>, salt: u32) -> f32 {
    return f32(hash_cell(cell, salt) >> 8u) * (1.0 / 16777216.0);
}

fn hash_cell2(cell: vec2<i32>, salt: u32) -> vec2<f32> {
    return vec2<f32>(hash_cell01(cell, salt), hash_cell01(cell, salt ^ 0x68bc21ebu));
}

// Smooth world-space value noise is the deterministic equivalent of the
// reference project's clump texture. It controls a broad field, while the
// per-cell hash keeps neighbouring blades from becoming visibly uniform.
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

fn hm_load(ix: i32, iz: i32) -> f32 {
    let x = clamp(ix, 0, i32(terrain.hm_width) - 1);
    let z = clamp(iz, 0, i32(terrain.hm_height) - 1);
    return textureLoad(heightmap, vec2<i32>(x, z), 0).x;
}

fn sample_height(world_xz: vec2<f32>) -> f32 {
    let coord = (world_xz - terrain.world_origin) / terrain.terrain_grid;
    let cell = floor(coord);
    let f = coord - cell;
    let ix = i32(cell.x);
    let iz = i32(cell.y);
    let y00 = hm_load(ix, iz);
    let y01 = hm_load(ix + 1, iz);
    let y10 = hm_load(ix, iz + 1);
    let y11 = hm_load(ix + 1, iz + 1);
    if (f.x <= 1.0 - f.y) {
        return y00 + (y10 - y00) * f.y + (y01 - y00) * f.x;
    }
    return y10 + (y01 - y11) - (y10 - y11) * f.x - (y01 - y11) * f.y;
}

fn sample_normal(world_xz: vec2<f32>) -> vec3<f32> {
    let s = terrain.terrain_grid;
    let hx0 = sample_height(world_xz - vec2<f32>(s, 0.0));
    let hx1 = sample_height(world_xz + vec2<f32>(s, 0.0));
    let hz0 = sample_height(world_xz - vec2<f32>(0.0, s));
    let hz1 = sample_height(world_xz + vec2<f32>(0.0, s));
    return normalize(vec3<f32>(-(hx1 - hx0), 2.0 * s, -(hz1 - hz0)));
}

@compute @workgroup_size(8, 8, 1)
fn cs_place(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= GRID_DIM || gid.y >= GRID_DIM || grass.enabled < 0.5) { return; }
    let half = f32(GRID_DIM) * 0.5;
    let snap = floor(frame.cam_pos.xz / grass.spacing) * grass.spacing;
    let cell = vec2<f32>(f32(gid.x) - half, f32(gid.y) - half);
    let cell_world = snap + cell * grass.spacing;
    let cell_id = vec2<i32>(floor(cell_world / grass.spacing));
    let seed = hash_cell01(cell_id, 0x4d2f91c3u);
    // ±0.925 cells: candidates may cross their nominal cell boundaries, so
    // the dense field has no obvious square lattice or marching rows.
    let jitter = (hash_cell2(cell_id, 0x19a8b437u) - vec2<f32>(0.5)) * (grass.spacing * 1.85);
    let world_xz = cell_world + jitter;
    let delta = world_xz - frame.cam_pos.xz;
    let field = clump_noise(world_xz, 0.075, 0xc7136d5bu);
    let coverage = grass.density * mix(1.0, mix(0.45, 1.35, field), grass.debug_flags.y);
    if (dot(delta, delta) > grass.near_radius * grass.near_radius || seed > coverage) { return; }
    let map_max = terrain.world_origin + vec2<f32>(f32(terrain.hm_width - 1u), f32(terrain.hm_height - 1u)) * terrain.terrain_grid;
    if (any(world_xz < terrain.world_origin) || any(world_xz >= map_max)) { return; }
    let geocell = clamp(vec2<i32>(floor((world_xz - terrain.world_origin) / terrain.land_grid)), vec2<i32>(0), vec2<i32>(i32(terrain.land_range) - 1));
    let geo = textureLoad(geography, geocell, 0).x;
    // C++ marks only cells whose base terrain texture is a named grass material.
    // This prevents the procedural pass from treating clear desert or dirt as grass.
    if ((geo & 0x80000000u) == 0u) { return; }
    // Exclude water, roads/tracks, forest areas, and hard building/obstacle
    // cells. Bit 2 (`full`) is intentionally NOT excluded: legacy Everon
    // marks broad normal ground with it, so rejecting it removes every blade
    // despite the surface being valid grass terrain.
    if (grass.debug_flags.x < 0.5 && (geo & 0x00000c7bu) != 0u) { return; }
    let y = sample_height(world_xz);
    if (y <= terrain.sea_level + 0.35) { return; }
    let normal = sample_normal(world_xz);
    if (normal.y < 0.70) { return; }
    let rel = vec3<f32>(world_xz.x, y, world_xz.y) - frame.cam_pos.xyz;
    let clip = frame.proj * frame.view * vec4<f32>(rel, 1.0);
    if (clip.w <= 0.0 || abs(clip.x) > clip.w * 1.15 || abs(clip.y) > clip.w * 1.15) { return; }
    let out_index = atomicAdd(&placement_count[0], 1u);
    if (out_index >= MAX_INSTANCES) { return; }
    instances[out_index].pos_seed = vec4<f32>(world_xz.x, y, world_xz.y, seed);
}

// The middle ring uses a stable half-density grid and a simplified two-segment
// crossed blade. It bridges the detailed cards and distant tufts without any
// camera-facing tile swap or frame-to-frame randomisation.
@compute @workgroup_size(8, 8, 1)
fn cs_place_mid(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= MID_GRID_DIM || gid.y >= MID_GRID_DIM || grass.enabled < 0.5) { return; }
    let mid_end = min(grass.far_radius, max(grass.near_radius + 10.0, min(160.0, grass.near_radius * 2.5)));
    if (mid_end <= grass.near_radius) { return; }
    let mid_spacing = max(grass.spacing * 2.0, mid_end / (f32(MID_GRID_DIM) * 0.5 - 1.0));
    let half = f32(MID_GRID_DIM) * 0.5;
    let snap = floor(frame.cam_pos.xz / mid_spacing) * mid_spacing;
    let cell = vec2<f32>(f32(gid.x) - half, f32(gid.y) - half);
    let cell_world = snap + cell * mid_spacing;
    let cell_id = vec2<i32>(floor(cell_world / mid_spacing));
    let seed = hash_cell01(cell_id, 0x37c4a51du);
    let jitter = (hash_cell2(cell_id, 0x9426d87bu) - vec2<f32>(0.5)) * (mid_spacing * 1.85);
    let world_xz = cell_world + jitter;
    let delta = world_xz - frame.cam_pos.xz;
    let distance2 = dot(delta, delta);
    let field = clump_noise(world_xz, 0.075, 0xc7136d5bu);
    let coverage = grass.density * 0.95 * mix(1.0, mix(0.45, 1.35, field), grass.debug_flags.y);
    // A small overlap avoids a bare annulus at the detailed/mid and mid/far joins.
    let mid_start = max(0.0, grass.near_radius - mid_spacing * 1.5);
    if (distance2 < mid_start * mid_start || distance2 > mid_end * mid_end || seed > coverage) { return; }
    let map_max = terrain.world_origin + vec2<f32>(f32(terrain.hm_width - 1u), f32(terrain.hm_height - 1u)) * terrain.terrain_grid;
    if (any(world_xz < terrain.world_origin) || any(world_xz >= map_max)) { return; }
    let geocell = clamp(vec2<i32>(floor((world_xz - terrain.world_origin) / terrain.land_grid)), vec2<i32>(0), vec2<i32>(i32(terrain.land_range) - 1));
    let geo = textureLoad(geography, geocell, 0).x;
    if ((geo & 0x80000000u) == 0u ||
        (grass.debug_flags.x < 0.5 && (geo & 0x00000c7bu) != 0u)) { return; }
    let y = sample_height(world_xz);
    if (y <= terrain.sea_level + 0.35 || sample_normal(world_xz).y < 0.70) { return; }
    let rel = vec3<f32>(world_xz.x, y, world_xz.y) - frame.cam_pos.xyz;
    let clip = frame.proj * frame.view * vec4<f32>(rel, 1.0);
    if (clip.w <= 0.0 || abs(clip.x) > clip.w * 1.15 || abs(clip.y) > clip.w * 1.15) { return; }
    let out_index = atomicAdd(&placement_count[0], 1u);
    if (out_index >= MAX_MID_INSTANCES) { return; }
    instances[out_index].pos_seed = vec4<f32>(world_xz.x, y, world_xz.y, seed);
}

// Coarse outer ring: minimum 1m cells, quarter density, and one triangle per
// survivor. This is the GPU equivalent of the reference project's far tiles,
// without its visible CPU tile relocation or hard mesh swaps at tile borders.
@compute @workgroup_size(8, 8, 1)
fn cs_place_far(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= FAR_GRID_DIM || gid.y >= FAR_GRID_DIM || grass.enabled < 0.5) { return; }
    // Keep the fixed 384x384 far candidate grid bounded even at the 5 km
    // developer radius: farther fields automatically use wider, cheaper cells.
    let far_spacing = max(max(grass.spacing * 4.0, 1.0), grass.far_radius / (f32(FAR_GRID_DIM) * 0.5 - 1.0));
    let half = f32(FAR_GRID_DIM) * 0.5;
    let snap = floor(frame.cam_pos.xz / far_spacing) * far_spacing;
    let cell = vec2<f32>(f32(gid.x) - half, f32(gid.y) - half);
    let cell_world = snap + cell * far_spacing;
    let cell_id = vec2<i32>(floor(cell_world / far_spacing));
    let seed = hash_cell01(cell_id, 0xb1e4c025u);
    let jitter = (hash_cell2(cell_id, 0x7b32d119u) - vec2<f32>(0.5)) * (far_spacing * 1.85);
    let world_xz = cell_world + jitter;
    let delta = world_xz - frame.cam_pos.xz;
    let distance2 = dot(delta, delta);
    let field = clump_noise(world_xz, 0.075, 0xc7136d5bu);
    let coverage = max(grass.density * 0.85, 0.60) * mix(1.0, mix(0.55, 1.25, field), grass.debug_flags.y);
    // Start one coarse cell after the near ring; this avoids double-drawing
    // while keeping the LOD join visually continuous.
    let mid_end = min(grass.far_radius, max(grass.near_radius + 10.0, min(160.0, grass.near_radius * 2.5)));
    let near_start = mid_end + far_spacing * 0.5;
    // Keep the outer ring visually continuous. It is still economical (one
    // triangle per tuft), but no longer becomes invisible just past 60 m.
    if (distance2 <= near_start * near_start || distance2 > grass.far_radius * grass.far_radius || seed > coverage) { return; }
    let map_max = terrain.world_origin + vec2<f32>(f32(terrain.hm_width - 1u), f32(terrain.hm_height - 1u)) * terrain.terrain_grid;
    if (any(world_xz < terrain.world_origin) || any(world_xz >= map_max)) { return; }
    let geocell = clamp(vec2<i32>(floor((world_xz - terrain.world_origin) / terrain.land_grid)), vec2<i32>(0), vec2<i32>(i32(terrain.land_range) - 1));
    let geo = textureLoad(geography, geocell, 0).x;
    if ((geo & 0x80000000u) == 0u ||
        (grass.debug_flags.x < 0.5 && (geo & 0x00000c7bu) != 0u)) { return; }
    let y = sample_height(world_xz);
    if (y <= terrain.sea_level + 0.35) { return; }
    let normal = sample_normal(world_xz);
    if (normal.y < 0.70) { return; }
    let rel = vec3<f32>(world_xz.x, y, world_xz.y) - frame.cam_pos.xyz;
    let clip = frame.proj * frame.view * vec4<f32>(rel, 1.0);
    if (clip.w <= 0.0 || abs(clip.x) > clip.w * 1.15 || abs(clip.y) > clip.w * 1.15) { return; }
    let out_index = atomicAdd(&placement_count[0], 1u);
    if (out_index >= MAX_FAR_INSTANCES) { return; }
    instances[out_index].pos_seed = vec4<f32>(world_xz.x, y, world_xz.y, seed);
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_rel: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) height_t: f32,
    @location(3) seed: f32,
};

@vertex
fn vs_grass(@builtin(vertex_index) vertex_index: u32, @builtin(instance_index) instance_index: u32) -> VsOut {
    let inst = instances[instance_index].pos_seed;
    let seed = inst.w;
    let field = clump_noise(inst.xz, 0.075, 0x48ac2f19u);
    let angle = mix(seed * 6.2831853, field * 6.2831853, grass.debug_flags.y);
    let side = vec3<f32>(cos(angle), 0.0, -sin(angle));
    let forward = vec3<f32>(sin(angle), 0.0, cos(angle));
    let height_seed = mix(hash11(inst.xz + 2.0), clump_noise(inst.xz, 0.21, 0xa47f3cd1u), grass.debug_flags.y * 0.72);
    let height = mix(0.35, 1.05, height_seed) * grass.blade_height;
    let width = mix(0.018, 0.045, hash11(inst.xz + 9.0));
    let wind_angle = grass.wind_direction * 0.01745329252;
    let wind_dir = vec2<f32>(cos(wind_angle), sin(wind_angle));
    let macro_gust = sin(dot(inst.xz, wind_dir * 0.065) + terrain.time * (0.85 + grass.wind_strength * 0.55) + seed * 6.28);
    // A height-shifted second wave gives the tip small independent turbulence,
    // like the reference shader, while the root stays completely locked.
    let local_gust = sin(dot(inst.xz, vec2<f32>(0.11, -0.08)) + terrain.time * 2.1 + seed * 19.3);
    let static_bend = forward * mix(0.055, 0.19, hash11(inst.xz + 31.0));
    let wind_bend = vec3<f32>(wind_dir.x, 0.0, wind_dir.y) * grass.wind_strength *
        (0.035 + 0.14 * macro_gust + 0.035 * local_gust);
    let interactor_delta = inst.xz - vec2<f32>(grass.interactor_x, grass.interactor_z);
    let interactor_distance = length(interactor_delta);
    var crush = 0.0;
    if (grass.interactor_radius > 0.01) {
        crush = (1.0 - smoothstep(grass.interactor_radius * 0.25, grass.interactor_radius, interactor_distance)) *
            grass.interactor_strength;
    }
    var crush_dir = select(vec2<f32>(0.0), interactor_delta / interactor_distance, interactor_distance > 0.001);
    // Reapply recent contact stamps.  They fade from strongly flattened to
    // fully recovered between 12 and 28 seconds, leaving a readable trail.
    for (var i = 0u; i < 96u; i = i + 1u) {
        let track = grass.tracks[i];
        if (track.radius <= 0.01 || track.age >= 60.0) { continue; }
        let delta = inst.xz - vec2<f32>(track.x, track.z);
        let distance = length(delta);
        let imprint = (1.0 - smoothstep(track.radius * 0.20, track.radius, distance)) *
            (1.0 - smoothstep(25.0, 60.0, track.age));
        if (imprint > crush) {
            crush = imprint;
            crush_dir = select(vec2<f32>(0.0), delta / distance, distance > 0.001);
        }
    }
    let crush_bend = vec3<f32>(crush_dir.x, 0.0, crush_dir.y) * height * (0.42 * crush);
    let crushed_height = height * (1.0 - 0.78 * crush);
    // Two crossed ribbons, each built from five quads. Every vertex samples
    // its own height along a quadratic curve rather than simply moving a tip.
    let card = vertex_index / VERTS_PER_CARD;
    let packed = vertex_index % VERTS_PER_CARD;
    let segment = packed / 6u;
    let corner = packed % 6u;
    let upper = corner == 2u || corner == 4u || corner == 5u;
    let left = corner == 0u || corner == 3u || corner == 5u;
    let t = f32(segment + select(0u, 1u, upper)) / f32(BLADE_SEGMENTS);
    let blade_axis = select(side, forward, card != 0u);
    let taper = pow(max(1.0 - t, 0.0), 0.65);
    let half_width = width * taper;
    let curve = (static_bend + wind_bend + crush_bend) * (t * t);
    let tangent = vec3<f32>(0.0, crushed_height, 0.0) + (static_bend + wind_bend + crush_bend) * (2.0 * t);
    // Preserve a minimum apparent silhouette when a card is nearly edge-on
    // to the camera. This mirrors the reference's view-space widening without
    // reconstructing a second model matrix or making distant ribbons explode.
    let blade_normal = normalize(cross(blade_axis, tangent));
    let view_dir = normalize(frame.cam_pos.xyz - inst.xyz);
    let edge_on = pow(1.0 - abs(dot(blade_normal, view_dir)), 4.0);
    let lateral = select(blade_axis * half_width, -blade_axis * half_width, left) * (1.0 + edge_on * 1.6);
    let local = lateral + vec3<f32>(0.0, crushed_height * t, 0.0) + curve;
    let world = inst.xyz + local;
    let rel = world - frame.cam_pos.xyz;
    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * vec4<f32>(rel, 1.0));
    out.world_rel = rel;
    out.normal = normalize(cross(blade_axis, tangent));
    out.height_t = t;
    out.seed = seed;
    return out;
}

@vertex
fn vs_grass_mid(@builtin(vertex_index) vertex_index: u32, @builtin(instance_index) instance_index: u32) -> VsOut {
    let inst = instances[instance_index].pos_seed;
    let seed = inst.w;
    let field = clump_noise(inst.xz, 0.075, 0x48ac2f19u);
    let angle = mix(seed * 6.2831853, field * 6.2831853, grass.debug_flags.y);
    let side = vec3<f32>(cos(angle), 0.0, -sin(angle));
    let forward = vec3<f32>(sin(angle), 0.0, cos(angle));
    let height_seed = mix(hash11(inst.xz + 13.0), clump_noise(inst.xz, 0.21, 0xa47f3cd1u), grass.debug_flags.y * 0.72);
    let height = mix(0.32, 0.92, height_seed) * grass.blade_height;
    let width = mix(0.024, 0.055, hash11(inst.xz + 23.0));
    let wind_angle = grass.wind_direction * 0.01745329252;
    let wind_dir = vec2<f32>(cos(wind_angle), sin(wind_angle));
    let gust = sin(dot(inst.xz, wind_dir * 0.065) + terrain.time * (0.85 + grass.wind_strength * 0.55) + seed * 6.28);
    let bend = (forward * mix(0.04, 0.14, hash11(inst.xz + 71.0)) +
        vec3<f32>(wind_dir.x, 0.0, wind_dir.y) * grass.wind_strength * (0.03 + 0.12 * gust));
    var crush = 0.0;
    var crush_dir = vec2<f32>(0.0);
    for (var i = 0u; i < 96u; i = i + 1u) {
        let track = grass.tracks[i];
        if (track.radius <= 0.01 || track.age >= 60.0) { continue; }
        let delta = inst.xz - vec2<f32>(track.x, track.z);
        let distance = length(delta);
        let imprint = (1.0 - smoothstep(track.radius * 0.20, track.radius, distance)) *
            (1.0 - smoothstep(25.0, 60.0, track.age));
        if (imprint > crush) {
            crush = imprint;
            crush_dir = select(vec2<f32>(0.0), delta / distance, distance > 0.001);
        }
    }
    let crush_bend = vec3<f32>(crush_dir.x, 0.0, crush_dir.y) * height * (0.42 * crush);
    let crushed_height = height * (1.0 - 0.78 * crush);
    let card = vertex_index / 12u;
    let packed = vertex_index % 12u;
    let segment = packed / 6u;
    let corner = packed % 6u;
    let upper = corner == 2u || corner == 4u || corner == 5u;
    let left = corner == 0u || corner == 3u || corner == 5u;
    let t = f32(segment + select(0u, 1u, upper)) * 0.5;
    let blade_axis = select(side, forward, card != 0u);
    let curve = (bend + crush_bend) * (t * t);
    let tangent = vec3<f32>(0.0, crushed_height, 0.0) + (bend + crush_bend) * (2.0 * t);
    let blade_normal = normalize(cross(blade_axis, tangent));
    let view_dir = normalize(frame.cam_pos.xyz - inst.xyz);
    let edge_on = pow(1.0 - abs(dot(blade_normal, view_dir)), 4.0);
    let lateral = select(blade_axis, -blade_axis, left) * width * pow(max(1.0 - t, 0.0), 0.7) * (1.0 + edge_on * 1.25);
    let rel = inst.xyz + lateral + vec3<f32>(0.0, crushed_height * t, 0.0) + curve - frame.cam_pos.xyz;
    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * vec4<f32>(rel, 1.0));
    out.world_rel = rel;
    out.normal = normalize(cross(blade_axis, tangent));
    out.height_t = t;
    out.seed = seed;
    return out;
}

@vertex
fn vs_grass_far(@builtin(vertex_index) vertex_index: u32, @builtin(instance_index) instance_index: u32) -> VsOut {
    let inst = instances[instance_index].pos_seed;
    let seed = inst.w;
    let field = clump_noise(inst.xz, 0.075, 0x48ac2f19u);
    let angle = mix(seed * 6.2831853, field * 6.2831853, grass.debug_flags.y);
    let side = vec3<f32>(cos(angle), 0.0, -sin(angle));
    let height_seed = mix(hash11(inst.xz + 43.0), clump_noise(inst.xz, 0.21, 0xa47f3cd1u), grass.debug_flags.y * 0.72);
    let height = mix(0.28, 0.78, height_seed) * grass.blade_height;
    // Larger, crossed-looking tufts at long range maintain field coverage;
    // this makes a 100–5000 m radius visibly meaningful instead of producing
    // isolated pin-thin triangles beyond the dense near ring.
    let distance01 = clamp(length(inst.xz - frame.cam_pos.xz) / max(grass.far_radius, 1.0), 0.0, 1.0);
    let width = mix(0.16, 0.58, distance01) * mix(0.75, 1.25, hash11(inst.xz + 91.0));
    let wind_angle = grass.wind_direction * 0.01745329252;
    let wind_dir = vec2<f32>(cos(wind_angle), sin(wind_angle));
    let gust = sin(dot(inst.xz, wind_dir * 0.065) + terrain.time * (0.85 + grass.wind_strength * 0.55) + seed * 6.28);
    let bend = vec3<f32>(wind_dir.x, 0.0, wind_dir.y) * grass.wind_strength * (0.025 + 0.10 * gust);
    var local: vec3<f32>;
    var t = 0.0;
    if (vertex_index == 0u) { local = -side * width; }
    else if (vertex_index == 1u) { local = side * width; }
    else { local = vec3<f32>(0.0, height, 0.0) + bend; t = 1.0; }
    let rel = inst.xyz + local - frame.cam_pos.xyz;
    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * vec4<f32>(rel, 1.0));
    out.world_rel = rel;
    out.normal = normalize(cross(side, normalize(vec3<f32>(0.0, height, 0.0) + bend)));
    out.height_t = t;
    out.seed = seed;
    return out;
}

@fragment
fn fs_grass(in: VsOut) -> @location(0) vec4<f32> {
    let world = in.world_rel + frame.cam_pos.xyz;
    let base = vec3<f32>(0.055, 0.16, 0.025);
    let tip = vec3<f32>(0.32, 0.43, 0.10);
    let root = mix(0.30, 1.0, in.height_t * in.height_t);
    let field_tint = clump_noise(world.xz, 0.16, 0x5e3a91c7u);
    let blade_tint = mix(in.seed, field_tint, 0.55);
    let variation = mix(1.0, mix(0.78, 1.20, blade_tint), grass.debug_flags.z);
    let albedo = mix(base, tip, in.height_t) * root * variation;
    // Bend card normals toward an upright rounded-blade normal. This avoids
    // the flat dark-side look of a raw ribbon while preserving its silhouette.
    let n = normalize(mix(normalize(in.normal), vec3<f32>(0.0, 1.0, 0.0), 0.24));
    let light_dir = normalize(frame.sun_dir_world.xyz);
    let ndl = max(dot(n, light_dir), 0.0);
    let wrap = max((dot(n, light_dir) + 0.35) / 1.35, 0.0);
    let terrain_shadow = terrain_sun_shadow(world.xz, world.y);
    let direct = frame.sun_diffuse.rgb * mix(ndl, wrap, 0.35) * (1.0 - terrain_shadow);
    let ambient = sky_irradiance(normalize(mix(n, vec3<f32>(0.0, 1.0, 0.0), 0.45))) * sky_vis_ao(world.xz);
    let view_dir = normalize(-in.world_rel);
    let transmission = pow(max(dot(view_dir, -light_dir), 0.0), 1.5) * (1.0 - ndl) * grass.debug_flags.w;
    let subsurface = vec3<f32>(1.0, 0.72, 0.18) * transmission * frame.sun_diffuse.rgb * (1.0 - terrain_shadow);
    return vec4<f32>(apply_fog(albedo * (ambient + direct + subsurface), in.world_rel), 1.0);
}

@fragment
fn fs_grass_prepass(in: VsOut) -> @location(0) vec2<f32> {
    let normal_view = (frame.view * vec4<f32>(normalize(in.normal), 0.0)).xyz;
    return oct_encode(normalize(normal_view));
}
