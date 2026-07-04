// Cascade shadow depth pass, one layer per cascade. Depth-only; the alpha
// variants discard below the cutout threshold (foliage silhouettes). Skinned
// entries pose the caster from its bone palette (world pre-multiplied in);
// group(1) declarations coexist because each entry point uses only one.
// ShadowMath conventions: camera-relative positions, NDC z in [0, 1], linear
// ortho depth — forward convention (clear 1.0, LessEqual), no reversed-Z.

struct PassData {
    light_vp: mat4x4<f32>,
};

struct CasterData {
    world: mat4x4<f32>, // camera-relative
    params: vec4<f32>,  // x = alpha_ref
};

struct Palette {
    m: array<mat4x4<f32>, 128>,
};

@group(0) @binding(0) var<uniform> pass_data: PassData;
@group(1) @binding(0) var<uniform> caster: CasterData;  // rigid pipelines
@group(1) @binding(0) var<uniform> palette: Palette;    // skinned pipelines
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

// Skinned pipelines have no caster UBO slot; the cutout threshold is baked.
override skin_alpha_ref: f32 = 0.5;

@vertex
fn vs_solid(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return pass_data.light_vp * caster.world * vec4<f32>(pos, 1.0);
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
) -> VsAlphaOut {
    var out: VsAlphaOut;
    out.clip = pass_data.light_vp * caster.world * vec4<f32>(pos, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_alpha(in: VsAlphaOut) {
    if (textureSample(tex, samp, in.uv).a < caster.params.x) {
        discard;
    }
}

fn skin_pos(pos: vec3<f32>, bones: vec4<u32>, weights: vec4<f32>) -> vec4<f32> {
    let p = vec4<f32>(pos, 1.0);
    return weights.x * (palette.m[bones.x] * p)
         + weights.y * (palette.m[bones.y] * p)
         + weights.z * (palette.m[bones.z] * p)
         + weights.w * (palette.m[bones.w] * p);
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
