// MSAA depth resolve (docs: MSAA path). WebGPU cannot resolve a depth attachment via a
// render-pass resolve_target, so this fullscreen pass reads the multisampled scene depth and
// writes a single-sample depth target the Hi-Z build (+ future SSAO / depth-based water opacity)
// can sample like the 1x depth aspect. It takes the NEAREST sample (max under reversed-Z, where
// near = 1 and far = 0) so the resolved depth stays conservative at geometry edges — nothing that
// is visible in any sample is pushed behind the resolved surface.

// Sample count as a spec constant so the per-sample loop unrolls at pipeline creation.
override sample_count: i32 = 4;

@group(0) @binding(0) var src: texture_depth_multisampled_2d;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Oversized fullscreen triangle covering the viewport.
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @builtin(frag_depth) f32 {
    // The resolved pixel maps 1:1 to the source pixel, so load by the framebuffer coordinate.
    let p = vec2<i32>(pos.xy);
    var d = 0.0; // reversed-Z far
    for (var s = 0; s < sample_count; s = s + 1) {
        d = max(d, textureLoad(src, p, s));
    }
    return d;
}
