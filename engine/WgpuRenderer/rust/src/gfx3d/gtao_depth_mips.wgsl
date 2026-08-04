// Linear view-Z mip chain for the GTAO horizon march (screen-space-ao-plan §4.3 / Stage 3).
//
// WHY NOT REUSE THE Hi-Z PYRAMID. hiz.wgsl exists and is also a depth mip chain, but it is a
// `min` over REVERSED-Z, i.e. the FARTHEST surface in each block — correct for occlusion
// culling, which must never cull something that might be visible. AO wants the exact opposite:
// the NEAREST surface, because that is the thing most likely to occlude. Sampling the culling
// pyramid here would systematically under-occlude, worse the coarser the mip, and would read as
// AO quietly fading out with distance.
//
// WHY LINEAR Z AND NOT STORED DEPTH. Reversed-Z is wildly non-linear, so averaging or reducing
// it across a block is not a depth in any useful sense. Linear view Z reduces meaningfully, and
// it also removes the per-tap matrix multiply: with a perspective projection a view position is
// just `(ndc.x / proj_xx, ndc.y / proj_yy, 1) * z`.
//
// Sky (cleared depth) is stored as a large sentinel rather than 0 so it can never win the `min`
// and invent an occluder at a silhouette. A block that is part sky, part surface reduces to the
// surface, which is what horizon search wants.

struct MipParams {
    // x = camera near plane (linearise: z = near / stored_depth), y..w unused.
    proj: vec4<f32>,
};

// Sky / nothing-drawn. Far enough that `min` never picks it and the thickness falloff rejects
// it if it ever survives, but not so far that it costs precision in an f32.
const SKY_Z: f32 = 1.0e7;

@group(0) @binding(0) var src_depth: texture_depth_2d;
@group(0) @binding(1) var dst0: texture_storage_2d<r32float, write>;
@group(0) @binding(2) var<uniform> params: MipParams;

// mip0: linearise the reversed-Z prepass depth into view-space metres.
//
// The engine's projection is FORWARD with an infinite far plane, and frame::reverse_z flips z
// in the vertex shader afterwards, so what lands in the depth buffer is exactly `near / z`.
// Inverting that is a divide, not a matrix — see gtao.wgsl for the longer note on the
// convention and on what feeding the raw stored value into inv(proj) does.
@compute @workgroup_size(8, 8)
fn cs_linearise(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(dst0));
    let px = vec2<i32>(gid.xy);
    if (px.x >= dims.x || px.y >= dims.y) {
        return;
    }
    let d = textureLoad(src_depth, px, 0);
    // Cleared reversed-Z is 0 = far = nothing drawn.
    let z = select(SKY_Z, params.proj.x / max(d, 1e-9), d > 0.0);
    textureStore(dst0, px, vec4<f32>(z, 0.0, 0.0, 0.0));
}

// reduce: 2x2 MIN of linear z (nearest surface wins), with the odd-dimension edge folded in so
// no occluder is dropped — the same edge handling hiz.wgsl needs, for the same reason.
@group(0) @binding(3) var src: texture_2d<f32>;
@group(0) @binding(4) var dst: texture_storage_2d<r32float, write>;

@compute @workgroup_size(8, 8)
fn cs_reduce(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dst_dims = vec2<i32>(textureDimensions(dst));
    let px = vec2<i32>(gid.xy);
    if (px.x >= dst_dims.x || px.y >= dst_dims.y) {
        return;
    }
    let src_dims = vec2<i32>(textureDimensions(src));
    let maxc = src_dims - vec2<i32>(1, 1);
    let base = px * 2;
    var m = textureLoad(src, min(base, maxc), 0).r;
    m = min(m, textureLoad(src, min(base + vec2<i32>(1, 0), maxc), 0).r);
    m = min(m, textureLoad(src, min(base + vec2<i32>(0, 1), maxc), 0).r);
    m = min(m, textureLoad(src, min(base + vec2<i32>(1, 1), maxc), 0).r);
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
    textureStore(dst, px, vec4<f32>(m, 0.0, 0.0, 0.0));
}
