// MSAA view-space-normal resolve (docs/screen-space-ao-plan.md §2).
//
// The depth+normal prepass writes an oct-encoded view-space normal into an Rg16Float target
// created at the scene's sample count. GTAO runs per pixel, so it needs a single-sample
// normal; unlike colour there is no automatic resolve for it, because the correct reduction
// is not an average.
//
// This takes SAMPLE 0 rather than averaging, and that is deliberate, not a shortcut:
// averaging the raw Rg texels is WRONG. Octahedral encoding wraps — two samples either side
// of the fold have codes at opposite ends of the range whose mean points nowhere near either
// normal. A correct average would have to decode every sample, average the directions,
// renormalise and re-encode. Sample 0 is what the plan prescribes to start with, per-pixel AO
// tolerates it, and the bilateral blur that follows GTAO hides the rest. If foliage or other
// high-coverage edges shimmer under motion, upgrade to the decode/average/re-encode form —
// that is the known next step, not an unknown.
//
// Depth deliberately uses a different reduction (see depth_resolve.wgsl): for depth the
// per-sample min/max is meaningful, so it reduces properly rather than picking a sample.

@group(0) @binding(0) var src: texture_multisampled_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Oversized fullscreen triangle covering the viewport, matching depth_resolve.
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // The resolved pixel maps 1:1 to the source pixel, so load by framebuffer coordinate.
    let p = vec2<i32>(pos.xy);
    return textureLoad(src, p, 0);
}
