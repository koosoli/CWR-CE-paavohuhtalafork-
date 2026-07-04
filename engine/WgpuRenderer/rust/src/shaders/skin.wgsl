#define_import_path skin

// Linear-blend GPU skinning shared by the lit mesh pipeline (shader3d) and the
// shadow depth pass. Both bind the bone palette at group(1)/binding(0), so the
// binding lives here once. 128 = the engine's own bone-palette cap
// (MATRIX_4_ARRAY(matrix, 128)); the palette has the per-object world
// pre-multiplied in (palette[i] = world * bone[i]).
struct Palette {
    m: array<mat4x4<f32>, 128>,
};

@group(1) @binding(0) var<uniform> palette: Palette;

// Up to 4 bone indices + normalised weights. Vertices with no skin weight carry
// a single weight of 1.0 on a reserved bone whose palette entry is just `world`,
// so no zero-weight fallback is needed.
fn skin_pos(pos: vec3<f32>, bones: vec4<u32>, weights: vec4<f32>) -> vec4<f32> {
    let p = vec4<f32>(pos, 1.0);
    return weights.x * (palette.m[bones.x] * p)
         + weights.y * (palette.m[bones.y] * p)
         + weights.z * (palette.m[bones.z] * p)
         + weights.w * (palette.m[bones.w] * p);
}

fn skin_normal(norm: vec3<f32>, bones: vec4<u32>, weights: vec4<f32>) -> vec3<f32> {
    let n = vec4<f32>(norm, 0.0);
    return weights.x * (palette.m[bones.x] * n).xyz
         + weights.y * (palette.m[bones.y] * n).xyz
         + weights.z * (palette.m[bones.z] * n).xyz
         + weights.w * (palette.m[bones.w] * n).xyz;
}
