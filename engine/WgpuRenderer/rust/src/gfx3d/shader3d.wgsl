// Rudimentary unlit textured 3D: MVP transform + a single texture sample.

// Frame-global scalars sharing the camera UBO (no room for a 5th bind group).
struct FrameParams {
    fog_start: f32,
    fog_inv_range: f32,
    fog_enabled: f32, // 0 = off, 1 = on
    shadow_strength: f32,
};

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    params: FrameParams,
};

struct Object {
    world: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<uniform> object: Object;
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

// Baked per-pipeline (pipeline-overridable constants): the alpha-test cutout
// threshold and whether this is a shadow-darken pipeline. Keeping them out of a
// per-draw binding avoids a 5th bind group (wgpu's default maxBindGroups is 4).
override alpha_ref: f32 = 0.0;   // discard fragments with alpha below this (0 = off)
override is_shadow: f32 = 0.0;   // 1 = output black + shadow-strength alpha
// Decal/overlay depth bias in reversed-NDC depth units, pulling the draw toward the
// camera. Applied in the vertex shader (not DepthBiasState, which is a no-op on the
// float depth format this backend gets) so roads/decals/overlays win the depth test
// against coplanar geometry.
override depth_bias: f32 = 0.0;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fog: f32, // 1 = keep colour, 0 = full fog
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VsOut {
    var out: VsOut;
    // object.world is camera-relative (its translation is offset by the camera
    // position on the C++ side), so length(world_pos.xyz) is the distance from
    // the camera. Mirrors GL33's VSTransform fog term.
    let world_pos = object.world * vec4<f32>(pos, 1.0);
    out.clip = frame.proj * frame.view * world_pos;
    // Reversed-Z: the shared projection is forward (near->0, far->1). Remap to
    // near->1, far->0 so the float depth buffer spends its exponent bits where
    // geometry actually is (far from 0), which massively improves precision at
    // range vs forward float depth. Pipelines use GreaterEqual + clear-to-0.
    out.clip.z = out.clip.w - out.clip.z;
    // Bias toward the camera (larger reversed depth) so decals/overlays win the
    // depth test against coplanar geometry.
    out.clip.z += depth_bias * out.clip.w;
    out.uv = uv;

    let dist = length(world_pos.xyz);
    let fog_factor = clamp(1.0 - (dist - frame.params.fog_start) * frame.params.fog_inv_range, 0.0, 1.0);
    out.fog = select(1.0, fog_factor, frame.params.fog_enabled > 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let base = textureSample(tex, samp, in.uv);
    if (base.a < alpha_ref) {
        discard;
    }
    if (is_shadow > 0.5) {
        return vec4<f32>(0.0, 0.0, 0.0, frame.params.shadow_strength * base.a);
    }
    // Blend toward the scene fog colour (matches GL33's mix(fogColor, r0, vFogTC)).
    let rgb = mix(frame.fog_color.rgb, base.rgb, in.fog);
    return vec4<f32>(rgb, base.a);
}
