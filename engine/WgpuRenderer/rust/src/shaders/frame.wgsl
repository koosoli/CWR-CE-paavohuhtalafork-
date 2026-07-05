#define_import_path frame

// Shared group(0): the per-frame camera/environment UBO plus the cascade shadow
// map + comparison sampler. Every 3D pipeline (lit meshes, terrain) binds this
// exact layout, so the structs and bindings live here once. The UBO global is
// deliberately named `frame` so importers can write `frame.proj` etc. unchanged.

struct FrameParams {
    fog_start: f32,
    fog_inv_range: f32,
    fog_enabled: f32, // 0 = off, 1 = on
    shadow_strength: f32,
};

struct ShadowBlock {
    cascade_vp: array<mat4x4<f32>, 4>,
    splits: vec4<f32>, // per-tier select distance (omni radius / frustum eye-depth)
    omni_radius: vec4<f32>,
    ctl: vec4<f32>,  // {count, omni_count, fade_range, bias_const}
    // `ctlb`, not `ctl2`: naga_oil forbids composable-module identifiers ending
    // in a digit (naga's namer reserves numeric suffixes for disambiguation).
    ctlb: vec4<f32>, // {texel_size, darkness, normal_offset_scale, pcf}
    cam_fwd: vec4<f32>,
    sun_dir: vec4<f32>,
};

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    params: FrameParams,
    shadow: ShadowBlock,
    cam_pos: vec4<f32>, // world-space camera position (used by the terrain pipeline)
    sun_diffuse: vec4<f32>, // sun light, accommodation folded in (terrain)
    sun_ambient: vec4<f32>,
    sun_dir_world: vec4<f32>, // main light's surface-to-light direction (terrain)
};

// One frame-global point or spot light. Positions are ABSOLUTE world space so a
// single upload serves every camera; the shader reconstructs the camera-relative
// offset via frame.cam_pos. Colours are pre-scaled by NightEffect on the CPU
// (fade out by day). Matches the C ABI WgrLight.
struct Light {
    pos: vec4<f32>,     // xyz = world-absolute position, w = start-attenuation distance
    diffuse: vec4<f32>, // rgb = diffuse * nightEffect
    ambient: vec4<f32>, // rgb = ambient * nightEffect
    dir: vec4<f32>,     // xyz = beam direction (spot), w = isSpot (1) else 0
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d_array;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
// Frame-global light store, shared by the lit-mesh + terrain pipelines. The
// active light count for this camera rides in frame.cam_pos.w (the buffer itself
// is a fixed capacity, so its length is not the count).
@group(0) @binding(3) var<storage, read> lights: array<Light>;

// Long-range terrain sun-shadow mask (terrain_shadow.wgsl compute sweep), promoted
// into the shared frame group so BOTH terrain and lit meshes sample it — that is
// how objects (infantry, buildings, aircraft) receive a mountain's cast shadow, not
// just the terrain. Per column it stores the world height below which that column is
// terrain-shadowed: .r = ceiling, .g = penumbra half-width (m), .b = strength; a
// point at (xz, y) is occluded by how far y sits below the ceiling. `map` takes a
// world-xz to the mask's [0,1] UV; enabled = 0 until a heightmap is loaded.
struct TerrainShadowMap {
    origin: vec2<f32>,     // world_origin
    inv_span: vec2<f32>,   // 1 / (hm_dims * terrain_grid): world-xz -> [0,1] over the map
    half_texel: vec2<f32>, // 0.5 / mask_dims
    enabled: f32,
    pad: f32,
};
@group(0) @binding(4) var terrain_shadow_mask: texture_2d<f32>;
@group(0) @binding(5) var terrain_shadow_samp: sampler;
@group(0) @binding(6) var<uniform> terrain_shadow_map: TerrainShadowMap;

// Occlusion [0,1] of the sun by terrain at world position (xz, y): 0 = lit, 1 =
// fully in terrain shadow. Zero when the feature is off or the point is off the map
// or above the shadow ceiling. Shared by the terrain and lit-mesh fragment shaders.
fn terrain_sun_shadow(world_xz: vec2<f32>, world_y: f32) -> f32 {
    if (terrain_shadow_map.enabled < 0.5) {
        return 0.0;
    }
    let uv = (world_xz - terrain_shadow_map.origin) * terrain_shadow_map.inv_span
             + terrain_shadow_map.half_texel;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return 0.0;
    }
    let sm = textureSampleLevel(terrain_shadow_mask, terrain_shadow_samp, uv, 0.0);
    let lit = smoothstep(sm.r - sm.g, sm.r + sm.g + 1e-3, world_y);
    return clamp(sm.b * (1.0 - lit), 0.0, 1.0);
}

// Reversed-Z: the shared projection is forward (near->0, far->1). Remap to
// near->1, far->0 so the float depth buffer spends its exponent bits where
// geometry actually is (far from 0), which massively improves precision at range
// vs forward float depth. Pipelines use GreaterEqual + clear-to-0.
fn reverse_z(clip: vec4<f32>) -> vec4<f32> {
    var c = clip;
    c.z = c.w - c.z;
    return c;
}

// Scene fog blend factor in [0,1] for a camera distance: 1 = keep colour, 0 =
// full fog. Returns 1 unconditionally when fog is disabled.
fn fog_factor(dist: f32) -> f32 {
    let f = clamp(1.0 - (dist - frame.params.fog_start) * frame.params.fog_inv_range, 0.0, 1.0);
    return select(1.0, f, frame.params.fog_enabled > 0.5);
}
