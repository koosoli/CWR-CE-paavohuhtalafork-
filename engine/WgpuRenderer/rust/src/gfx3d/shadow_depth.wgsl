// Cascade shadow depth pass, one layer per cascade. Depth-only; the alpha
// variants discard below the cutout threshold (foliage silhouettes). Skinned
// entries pose the caster from its bone palette (world pre-multiplied in);
// group(1) declarations coexist because each entry point uses only one.
// ShadowMath conventions: camera-relative positions, NDC z in [0, 1], linear
// ortho depth — forward convention (clear 1.0, LessEqual), no reversed-Z.

#import skin::{skin_pos}
// Group(4) terrain heightmap + surface_y, shared with the lit mesh pass (shader3d),
// so the shadow silhouette conforms ClipLand vegetation to exactly the same ground.
#import conform::surface_y

struct PassData {
    light_vp: mat4x4<f32>,
    cam_pos: vec4<f32>, // camera world position; casters are camera-relative, so
                        // absolute world xz (for surface_y) = caster xz + cam_pos.xz
};

struct CasterData {
    world: mat4x4<f32>,  // camera-relative
    params: vec4<f32>,   // x = alpha_ref
    conform0: vec4<f32>, // x = bcSurfaceY (mode 2)
    conform2: vec4<f32>, // z = mode (0 = none, 2 = per-vertex ClipLand heightmap)
};

@group(0) @binding(0) var<uniform> pass_data: PassData;
@group(1) @binding(0) var<uniform> caster: CasterData;  // rigid pipelines
// Skinned pipelines bind the bone `palette` at this same group(1)/binding(0)
// slot (declared by the skin module); each entry point uses only one.
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

// Skinned pipelines have no caster UBO slot; the cutout threshold is baked.
override skin_alpha_ref: f32 = 0.5;

// Conform the camera-relative world position of a rigid caster vertex to the ground,
// mirroring vs_main's mode-2 branch (shader3d.wgsl) and Object::Animate exactly. Skinned
// casters are never terrain-conformed veg, so this is only used by the rigid entries.
fn conform_pos(world_pos: vec4<f32>, conform_sel: u32) -> vec4<f32> {
    var wp = world_pos;
    if (caster.conform2.z > 1.5) {
        let abs_x = wp.x + pass_data.cam_pos.x;
        let abs_z = wp.z + pass_data.cam_pos.z;
        let sy = surface_y(vec2<f32>(abs_x, abs_z));
        if (conform_sel == 1u) {
            // world.y_abs = SurfaceY + undeformedWorldY - bcSurfaceY (conform0.x); the
            // cam.y offset cancels between the two camera-relative terms.
            wp.y = sy + wp.y - caster.conform0.x;
        } else if (conform_sel == 2u) {
            wp.y = sy - pass_data.cam_pos.y;
        }
    }
    return wp;
}

@vertex
fn vs_solid(
    @location(0) pos: vec3<f32>,
    @location(5) conform_sel: u32,
) -> @builtin(position) vec4<f32> {
    let world_pos = conform_pos(caster.world * vec4<f32>(pos, 1.0), conform_sel);
    return pass_data.light_vp * world_pos;
}

struct VsAlphaOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_alpha(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(5) conform_sel: u32,
) -> VsAlphaOut {
    var out: VsAlphaOut;
    let world_pos = conform_pos(caster.world * vec4<f32>(pos, 1.0), conform_sel);
    out.clip = pass_data.light_vp * world_pos;
    out.uv = uv;
    return out;
}

@fragment
fn fs_alpha(in: VsAlphaOut) {
    if (textureSample(tex, samp, in.uv).a < caster.params.x) {
        discard;
    }
}

@vertex
fn vs_skin_solid(
    @location(0) pos: vec3<f32>,
    @location(3) bones: vec4<u32>,
    @location(4) weights: vec4<f32>,
) -> @builtin(position) vec4<f32> {
    return pass_data.light_vp * skin_pos(pos, bones, weights);
}

@vertex
fn vs_skin_alpha(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bones: vec4<u32>,
    @location(4) weights: vec4<f32>,
) -> VsAlphaOut {
    var out: VsAlphaOut;
    out.clip = pass_data.light_vp * skin_pos(pos, bones, weights);
    out.uv = uv;
    return out;
}

@fragment
fn fs_skin_alpha(in: VsAlphaOut) {
    if (textureSample(tex, samp, in.uv).a < skin_alpha_ref) {
        discard;
    }
}
