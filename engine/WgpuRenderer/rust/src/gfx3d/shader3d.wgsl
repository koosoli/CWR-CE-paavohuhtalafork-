// Rudimentary unlit textured 3D: MVP transform + a single texture sample.

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    fog_params: vec4<f32>, // {start, inv_range, enabled, pad}
};

struct Object {
    world: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var<uniform> object: Object;
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

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
    out.uv = uv;

    let dist = length(world_pos.xyz);
    let fog_factor = clamp(1.0 - (dist - frame.fog_params.x) * frame.fog_params.y, 0.0, 1.0);
    out.fog = select(1.0, fog_factor, frame.fog_params.z > 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let base = textureSample(tex, samp, in.uv);
    // Blend toward the scene fog colour (matches GL33's mix(fogColor, r0, vFogTC)).
    let rgb = mix(frame.fog_color.rgb, base.rgb, in.fog);
    return vec4<f32>(rgb, base.a);
}
