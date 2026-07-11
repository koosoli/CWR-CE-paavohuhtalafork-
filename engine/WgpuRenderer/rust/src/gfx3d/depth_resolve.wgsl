// MSAA depth resolve (docs: MSAA path). WebGPU cannot resolve a depth attachment via a
// render-pass resolve_target, so this fullscreen pass reads the multisampled scene depth and
// writes a single-sample depth target consumers can sample like the 1x depth aspect.
//
// The reduction is picked per consumer via the `reduce_far` override (reversed-Z: near = 1, far = 0):
//   - reduce_far = false → NEAREST sample (max): conservative for Hi-Z occlusion — nothing visible
//     in any sample is pushed behind the resolved surface.
//   - reduce_far = true  → FARTHEST sample (min): the real opaque seabed for water depth. A nearest
//     resolve would read A2C foliage / rotor edges as the seabed and ring them with foam; the
//     farthest sample skips those partial-coverage edges and keeps the true water column depth.

// Sample count as a spec constant so the per-sample loop unrolls at pipeline creation.
override sample_count: i32 = 4;
override reduce_far: i32 = 0; // 0 = nearest (max), 1 = farthest (min)

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
    let far = reduce_far != 0;
    var d = select(0.0, 1.0, far); // seed at the opposite extreme (max→far 0, min→near 1)
    for (var s = 0; s < sample_count; s = s + 1) {
        let sd = textureLoad(src, p, s);
        if (far) {
            d = min(d, sd);
        } else {
            d = max(d, sd);
        }
    }
    return d;
}
