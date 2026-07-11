// Hi-Z depth pyramid build (docs/gpu-culling-and-depth-plan.md §5).
//
// Reduces the depth prepass into a mip chain the color-pass cull samples for occlusion
// (cull.wgsl main_occlude). Reversed-Z convention throughout this backend: near = 1, far
// = 0, so occlusion wants the FARTHEST occluder over each region = the MINIMUM reversed-Z.
// Every reduction is therefore a `min` — getting this backwards silently culls everything
// (max keeps the near occluder, so every region reads "fully covered") or nothing. This is
// the headline correctness hazard of the whole feature.
//
// mip0 is a 1:1 copy of the scene depth (full resolution); each subsequent mip is a 2x2
// min of the previous, with the odd-dimension edge folded in so no occluder is lost (a
// skipped hole texel would raise the min and over-cull, dropping visible geometry).

// --- mip0: copy the scene depth (depth aspect) into the pyramid's top level ---
@group(0) @binding(0) var src_depth: texture_depth_2d;
@group(0) @binding(1) var dst0: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8)
fn copy_mip0(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst0);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let d = textureLoad(src_depth, vec2<i32>(gid.xy), 0);
    textureStore(dst0, vec2<i32>(gid.xy), vec4<f32>(d, 0.0, 0.0, 0.0));
}

// --- reduce: min 2x2 (+ odd-edge) of mip m-1 (src) into mip m (dst) ---
@group(0) @binding(2) var src: texture_2d<f32>;
@group(0) @binding(3) var dst: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8)
fn reduce(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_dims = textureDimensions(dst);
    if (gid.x >= dst_dims.x || gid.y >= dst_dims.y) {
        return;
    }
    let src_dims = vec2<i32>(textureDimensions(src));
    let maxc = src_dims - vec2<i32>(1, 1);
    let base = vec2<i32>(gid.xy) * 2;
    var m = textureLoad(src, min(base, maxc), 0).r;
    m = min(m, textureLoad(src, min(base + vec2<i32>(1, 0), maxc), 0).r);
    m = min(m, textureLoad(src, min(base + vec2<i32>(0, 1), maxc), 0).r);
    m = min(m, textureLoad(src, min(base + vec2<i32>(1, 1), maxc), 0).r);
    // Odd parent dimensions leave an unreduced last row/column this texel must cover.
    let odd_x = (src_dims.x & 1) != 0;
    let odd_y = (src_dims.y & 1) != 0;
    if (odd_x) {
        m = min(m, textureLoad(src, min(base + vec2<i32>(2, 0), maxc), 0).r);
        m = min(m, textureLoad(src, min(base + vec2<i32>(2, 1), maxc), 0).r);
    }
    if (odd_y) {
        m = min(m, textureLoad(src, min(base + vec2<i32>(0, 2), maxc), 0).r);
        m = min(m, textureLoad(src, min(base + vec2<i32>(1, 2), maxc), 0).r);
    }
    if (odd_x && odd_y) {
        m = min(m, textureLoad(src, min(base + vec2<i32>(2, 2), maxc), 0).r);
    }
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(m, 0.0, 0.0, 0.0));
}
