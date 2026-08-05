// Depth-only rasterisation of ONE model into ONE sky direction's map, for the per-model
// sky-visibility bake (docs/interior-sky-visibility-plan.md §3c). Paired with sky_bake.wgsl,
// which reduces the resulting maps into the model's visibility volume.
//
// Its own module rather than another entry point in sky_bake.wgsl because both want
// @group(0) @binding(0) for different things, and a WGSL module may only declare that once.
//
// Positions arrive in MODEL space and there is no instance transform — that is the whole point.
// The result is a property of the model, reusable by every instance of it at any position or
// rotation, which is what makes this affordable to compute at high quality exactly once.

@group(0) @binding(0) var<uniform> vp: mat4x4<f32>;

@vertex
fn vs_bake_depth(@location(0) pos: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vp * vec4<f32>(pos, 1.0);
}
