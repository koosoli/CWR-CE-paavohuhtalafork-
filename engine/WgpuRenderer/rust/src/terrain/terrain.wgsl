// GPU terrain: a shared grid mesh instanced per node, heightmap-displaced in the
// vertex shader. Shares group 0 (the camera UBO) with the lit 3D pipeline.

struct FrameParams {
    fog_start: f32,
    fog_inv_range: f32,
    fog_enabled: f32,
    shadow_strength: f32,
};

struct ShadowBlock {
    cascade_vp: array<mat4x4<f32>, 4>,
    splits: vec4<f32>,
    omni_radius: vec4<f32>,
    ctl: vec4<f32>,
    ctl2: vec4<f32>,
    cam_fwd: vec4<f32>,
    sun_dir: vec4<f32>,
};

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    params: FrameParams,
    shadow: ShadowBlock,
    cam_pos: vec4<f32>,
};

struct TerrainParams {
    world_origin: vec2<f32>,
    land_grid: f32,
    terrain_grid: f32,
    hm_width: u32,
    hm_height: u32,
    land_range: u32,
    data_scale: f32,
};

// Must match GRID_N in terrain/mod.rs.
const GRID_N: f32 = 32.0;

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<uniform> tp: TerrainParams;
@group(1) @binding(1) var heightmap: texture_2d<f32>;
@group(2) @binding(0) var ground: texture_2d_array<f32>;
@group(2) @binding(1) var ground_samp: sampler;

fn hm_load(ix: i32, iz: i32) -> f32 {
    let cx = clamp(ix, 0, i32(tp.hm_width) - 1);
    let cz = clamp(iz, 0, i32(tp.hm_height) - 1);
    return textureLoad(heightmap, vec2<i32>(cx, cz), 0).x;
}

// World height at a world-xz, matching Landscape::SurfaceY's per-cell triangle
// interpolation (not bilinear) so surface decals stay coplanar with the mesh.
fn sample_height(world_xz: vec2<f32>) -> f32 {
    let t = (world_xz - tp.world_origin) / tp.terrain_grid;
    let base = floor(t);
    let ix = i32(base.x);
    let iz = i32(base.y);
    let f = t - base; // f.x = x within cell, f.y = z within cell
    let y00 = hm_load(ix, iz);
    let y01 = hm_load(ix + 1, iz);
    let y10 = hm_load(ix, iz + 1);
    let y11 = hm_load(ix + 1, iz + 1);
    if (f.x <= 1.0 - f.y)
    {
        return y00 + (y10 - y00) * f.y + (y01 - y00) * f.x;
    }
    return y10 + (y01 - y11) - (y10 - y11) * f.x - (y01 - y11) * f.y;
}

// Central-difference normal. `step` scales with the patch's LOD (matching GL33's
// terrainGrid*lodStride) so lit detail tracks the tessellated detail.
fn sample_normal(world_xz: vec2<f32>, step: f32) -> vec3<f32> {
    let hx0 = sample_height(world_xz - vec2<f32>(step, 0.0));
    let hx1 = sample_height(world_xz + vec2<f32>(step, 0.0));
    let hz0 = sample_height(world_xz - vec2<f32>(0.0, step));
    let hz1 = sample_height(world_xz + vec2<f32>(0.0, step));
    return normalize(vec3<f32>(-(hx1 - hx0), 2.0 * step, -(hz1 - hz0)));
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_xz: vec2<f32>,  // absolute world-xz
    @location(1) fog: f32,             // 1 = keep colour, 0 = full fog
    @location(2) world_pos: vec3<f32>, // camera-relative
    @location(3) normal: vec3<f32>,    // world space, outward
};

// Skirt drop, as a multiple of the patch's vertex spacing.
const SKIRT_K: f32 = 4.0;

@vertex
fn vs_terrain(
    @location(0) grid_in: vec3<f32>, // xy = unit grid position in [0,1]^2, z = skirt flag
    @location(1) origin: vec2<f32>,  // node world-xz origin
    @location(2) size: f32,          // node world size
    @location(3) lod: u32,
    @location(4) morph: vec2<f32>,   // (morph_start, morph_end) camera-distance band
) -> VsOut {
    let grid = grid_in.xy;
    let world_xz_fine = origin + grid * size;
    let height_fine = sample_height(world_xz_fine);
    let dist = length(vec3<f32>(world_xz_fine.x, height_fine, world_xz_fine.y) - frame.cam_pos.xyz);

    // Snap toward the coarser even lattice as the vertex nears morph_end, so the
    // edge meets the parent grid at the LOD switch without a crack.
    var morph_k = 0.0;
    if (morph.y > morph.x)
    {
        morph_k = clamp((dist - morph.x) / (morph.y - morph.x), 0.0, 1.0);
    }
    let gidx = grid * GRID_N;
    let grid_coarse = (round(gidx * 0.5) * 2.0) / GRID_N;
    let world_xz = origin + mix(grid, grid_coarse, morph_k) * size;

    let height = sample_height(world_xz) - grid_in.z * (size / GRID_N) * SKIRT_K;
    let world_rel = vec3<f32>(world_xz.x, height, world_xz.y) - frame.cam_pos.xyz;

    var out: VsOut;
    out.clip = frame.proj * frame.view * vec4<f32>(world_rel, 1.0);
    // Reversed-Z: forward projection (near->0, far->1) remapped to near->1, far->0.
    out.clip.z = out.clip.w - out.clip.z;
    out.world_xz = world_xz;
    out.world_pos = world_rel;
    // Doubled as the patch morphs to coarse, so the normal step follows the mesh.
    let normal_step = (size / GRID_N) * (1.0 + morph_k);
    out.normal = sample_normal(world_xz, normal_step);

    let fog_dist = length(world_rel);
    let fog_factor = clamp(1.0 - (fog_dist - frame.params.fog_start) * frame.params.fog_inv_range, 0.0, 1.0);
    out.fog = select(1.0, fog_factor, frame.params.fog_enabled > 0.5);
    return out;
}

@fragment
fn fs_terrain(in: VsOut) -> @location(0) vec4<f32> {
    // Placeholder: one ground layer tiled per land cell + simple lambert.
    let uv = in.world_xz / tp.land_grid;
    var rgb = textureSample(ground, ground_samp, uv, 0).rgb;

    let n = normalize(in.normal);
    let sun = normalize(vec3<f32>(0.4, 0.85, 0.3));
    let lambert = max(dot(n, sun), 0.0) * 0.7 + 0.3;
    rgb *= lambert;

    rgb = mix(frame.fog_color.rgb, rgb, in.fog);
    return vec4<f32>(rgb, 1.0);
}
