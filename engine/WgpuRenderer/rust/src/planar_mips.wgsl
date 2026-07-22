@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.uv = uv;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_downsample(in: VsOut) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(src_tex));
    let a = textureSampleLevel(src_tex, src_samp, in.uv + texel * vec2<f32>(-0.5, -0.5), 0.0).rgb;
    let b = textureSampleLevel(src_tex, src_samp, in.uv + texel * vec2<f32>( 0.5, -0.5), 0.0).rgb;
    let c = textureSampleLevel(src_tex, src_samp, in.uv + texel * vec2<f32>(-0.5,  0.5), 0.0).rgb;
    let d = textureSampleLevel(src_tex, src_samp, in.uv + texel * vec2<f32>( 0.5,  0.5), 0.0).rgb;
    return vec4<f32>((a + b + c + d) * 0.25, 1.0);
}
