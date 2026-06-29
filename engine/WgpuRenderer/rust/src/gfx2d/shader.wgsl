struct Globals {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    // Pixel space (top-left, y down) -> NDC (centre, y up).
    let ndc = vec2<f32>(
        pos.x / globals.screen.x * 2.0 - 1.0,
        1.0 - pos.y / globals.screen.y * 2.0,
    );
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = uv;
    // color is in BGRA order -> swizzle to RGBA
    out.color = color.zyxw;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv) * in.color;
}
