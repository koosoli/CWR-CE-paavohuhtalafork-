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

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d_array;
@group(0) @binding(2) var shadow_samp: sampler_comparison;

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
