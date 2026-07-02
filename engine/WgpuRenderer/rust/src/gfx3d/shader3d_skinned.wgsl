// Skinned unlit textured 3D: linear-blend skinning + a single texture sample.
//
// The bone palette has the per-object world matrix pre-multiplied in on the C++
// side (palette[i] = world * boneMatrix[i]), so the skinned position is already
// in camera-relative world space and no separate world binding is needed. This
// keeps groups 0/2/3 (camera/texture/sampler) identical to the unlit pipeline;
// only group 1 differs (palette instead of a per-draw world matrix).
//
// Vertices carry up to 4 bone indices + normalised weights. Vertices with no
// skin weight get a single weight of 1.0 pointing at a reserved bone whose
// palette entry is just `world`, so this shader needs no zero-weight fallback.

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>, // {start, inv_range, enabled, pad}
};

// 128 = the engine's own bone-palette cap (MATRIX_4_ARRAY(matrix, 128)).
struct Palette {
    m: array<mat4x4<f32>, 128>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<uniform> palette: Palette;
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fog: f32,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bones: vec4<u32>,   // Uint8x4: palette indices
    @location(4) weights: vec4<f32>, // Unorm8x4: normalised weights
) -> VsOut {
    var out: VsOut;

    let p = vec4<f32>(pos, 1.0);
    var world_pos =
          weights.x * (palette.m[bones.x] * p)
        + weights.y * (palette.m[bones.y] * p)
        + weights.z * (palette.m[bones.z] * p)
        + weights.w * (palette.m[bones.w] * p);

    out.clip = frame.proj * frame.view * world_pos;
    out.uv = uv;

    let dist = length(world_pos.xyz);
    let fog_factor = clamp(1.0 - (dist - frame.fog_params.x) * frame.fog_params.y, 0.0, 1.0);
    out.fog = select(1.0, fog_factor, frame.fog_params.z > 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let base = textureSample(tex, samp, in.uv);
    let rgb = mix(frame.fog_color.rgb, base.rgb, in.fog);
    return vec4<f32>(rgb, base.a);
}
