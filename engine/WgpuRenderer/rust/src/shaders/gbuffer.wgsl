#define_import_path gbuffer

// Partial G-buffer helpers for the depth+normal prepass (docs/depth-prepass-plan.md).
// The prepass' one colour attachment is a view-space normal target, octahedral-encoded
// into Rg16Float — compact and banding-free, which the future SSAO/GTAO/SSR consumers
// need (precision matters for AO). Encode in the prepass fragment; a consumer decodes
// with oct_decode once one exists (none in Stage 1).

// sign() returns 0 at 0, which would collapse a hemisphere fold; force +1 there.
fn sign_nz(v: vec2<f32>) -> vec2<f32> {
    return select(vec2<f32>(-1.0), vec2<f32>(1.0), v >= vec2<f32>(0.0));
}

// Unit vector -> vec2 in [-1,1] (Cigolle et al. octahedral mapping).
fn oct_encode(n_in: vec3<f32>) -> vec2<f32> {
    let n = n_in / (abs(n_in.x) + abs(n_in.y) + abs(n_in.z));
    let folded = (1.0 - abs(vec2<f32>(n.y, n.x))) * sign_nz(n.xy);
    return select(n.xy, folded, n.z < 0.0);
}

// Inverse of oct_encode: vec2 in [-1,1] -> unit vector. Kept alongside the encoder so
// the first screen-space consumer has it ready.
fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    var v = vec3<f32>(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    if (v.z < 0.0) {
        let xy = (1.0 - abs(vec2<f32>(v.y, v.x))) * sign_nz(v.xy);
        v = vec3<f32>(xy.x, xy.y, v.z);
    }
    return normalize(v);
}
