// Per-model sky-visibility bake (docs/interior-sky-visibility-plan.md §3c, Stage 2).
//
// Two stages. First, the model's own geometry is rasterised into one depth map per sampled sky
// direction, in MODEL space. Then this compute walks the model's voxel grid and, for each voxel,
// asks every direction's map whether the sky is reachable — accumulating the cosine-weighted
// fraction into the volume that the object shader will later sample.
//
// Why this beats the per-frame camera-space map it replaces: the grid is fixed to the MODEL, so
// the boundary between "lit" and "occluded" lands on the building's own geometry instead of on a
// camera-relative texel lattice that has no relationship to the walls. That mismatch is what
// produced hard, geometry-unrelated shadow patches indoors, and no kernel width or bias tuning
// could fix it, because the information needed to align the boundary was never in the map.

struct BakeParams {
    // Model-space AABB the volume covers. `min` is voxel (0,0,0)'s corner.
    bbox_min: vec4<f32>,
    bbox_max: vec4<f32>,
    // Volume dimensions in voxels (xyz); w = the number of sampled directions.
    dims: vec4<u32>,
    // Depth bias in NDC units, applied to the receiver before the comparison, so a voxel sitting
    // exactly on a surface is not occluded by that surface.
    bias: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: BakeParams;
// One ortho view-projection per direction: MODEL space -> that direction's clip space.
@group(0) @binding(1) var<storage, read> dir_vp: array<mat4x4<f32>>;
// The directions themselves; xyz = toward the sky in model space, w = cosine weight.
@group(0) @binding(2) var<storage, read> dir_vec: array<vec4<f32>>;
// Depth maps, one array layer per direction, rasterised from the model's own geometry.
@group(0) @binding(3) var depth_maps: texture_depth_2d_array;
@group(0) @binding(4) var depth_samp: sampler_comparison;
// Output volume, linearised: x + y*dims.x + z*dims.x*dims.y. Per voxel: xyz = the average
// direction the sky arrives FROM (model space, unnormalised sum), w = the cosine-weighted
// fraction of sky that reaches it.
//
// The DIRECTION is what lets a room read as lit through its window rather than merely dimmer. It
// is baked rather than derived per frame for a reason found the hard way: deriving it from five
// per-frame directions makes the steered normal JUMP between them across a surface, and that
// quantisation is visible as hard shadow patches. Here it is integrated over 41 directions and
// then trilinearly filtered, so there is nothing to quantise.
//
// A storage BUFFER rather than a storage texture because R8Unorm is not a core storage format
// and this has to work on every adapter, not just the one it was written on.
@group(0) @binding(5) var<storage, read_write> out_vis: array<vec4<f32>>;

// Model-space centre of voxel (ix, iy, iz).
fn voxel_centre(i: vec3<u32>) -> vec3<f32> {
    let n = vec3<f32>(p.dims.xyz);
    let t = (vec3<f32>(i) + vec3<f32>(0.5)) / n;
    return mix(p.bbox_min.xyz, p.bbox_max.xyz, t);
}

@compute @workgroup_size(4, 4, 4)
fn cs_bake(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= p.dims.x || gid.y >= p.dims.y || gid.z >= p.dims.z) {
        return;
    }
    let pos = voxel_centre(gid);
    var num = 0.0;
    var den = 0.0;
    var dir_acc = vec3<f32>(0.0);
    for (var d = 0u; d < p.dims.w; d++) {
        let w = dir_vec[d].w;
        den += w;
        let clip = dir_vp[d] * vec4<f32>(pos, 1.0);
        // Orthographic, so w is 1 and clip is already NDC.
        let uv = vec2<f32>(clip.x * 0.5 + 0.5, -clip.y * 0.5 + 0.5);
        if (clip.z < 0.0 || clip.z > 1.0 || uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
            // Outside this direction's box: nothing of the model can block it, so the sky is
            // visible. Absence of data must never darken.
            num += w;
            dir_acc += dir_vec[d].xyz * w;
            continue;
        }
        // LessEqual compare: 1 when the voxel is at or in front of the stored occluder, i.e.
        // nothing of this model lies between the voxel and the sky along this direction.
        let vis = textureSampleCompareLevel(
            depth_maps, depth_samp, uv, i32(d), clip.z - p.bias.x);
        num += vis * w;
        dir_acc += dir_vec[d].xyz * (vis * w);
    }
    let idx = gid.x + gid.y * p.dims.x + gid.z * p.dims.x * p.dims.y;
    // Normalised so trilinear blending between voxels stays a direction; zero when nothing is
    // visible, which the sampler reads as "no opinion, use the surface normal".
    let dir = select(vec3<f32>(0.0), normalize(dir_acc), dot(dir_acc, dir_acc) > 1e-8);
    out_vis[idx] = vec4<f32>(dir, num / max(den, 1e-4));
}
