// Bilateral denoise for GTAO (docs/screen-space-ao-plan.md §4).
//
// This is the load-bearing step, not a polish pass. The project runs MSAA with NO TAA, so
// there is no history to accumulate the GTAO dither into — this blur is the entire denoise
// budget. If it is weakened, the interleaved-gradient noise in the AO becomes visible
// directly.
//
// Separable: dispatched twice, horizontal then vertical, with `axis` selecting. A separable
// pass is O(2N) taps instead of O(N²) and is what makes a radius wide enough to erase the
// dither affordable.
//
// Edge-aware on TWO signals, and both are needed:
//   - view-depth difference stops AO bleeding across silhouettes, which would otherwise draw
//     a dark halo around every object against the sky;
//   - normal divergence stops it bleeding across creases, so a wall-floor junction keeps its
//     contact darkening instead of smearing it flat.
// Depth alone is not enough: two surfaces meeting at a crease are at nearly the same depth.

struct BlurParams {
    // xy = size in px, zw = 1/size.
    screen: vec4<f32>,
    // x = axis (0 = horizontal, 1 = vertical), y = radius in taps,
    // z = depth rejection scale, w = normal rejection power.
    tuning: vec4<f32>,
};

@group(0) @binding(0) var depth_tex: texture_depth_2d;
@group(0) @binding(1) var normal_tex: texture_2d<f32>;
@group(0) @binding(2) var ao_in: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: BlurParams;
@group(0) @binding(4) var ao_out: texture_storage_2d<r8unorm, write>;

// Must match shaders/gbuffer.wgsl's oct_encode. Duplicated for the same reason gtao.wgsl
// duplicates it: include_str! module, no naga_oil composer.
fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    var v = vec3<f32>(e.xy, 1.0 - abs(e.x) - abs(e.y));
    if (v.z < 0.0) {
        let s = vec2<f32>(select(-1.0, 1.0, v.x >= 0.0), select(-1.0, 1.0, v.y >= 0.0));
        v = vec3<f32>((vec2<f32>(1.0) - abs(v.yx)) * s, v.z);
    }
    return normalize(v);
}

@compute @workgroup_size(8, 8, 1)
fn cs_gtao_blur(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(ao_out));
    let px = vec2<i32>(gid.xy);
    if (px.x >= dims.x || px.y >= dims.y) {
        return;
    }

    let d_centre = textureLoad(depth_tex, px, 0);
    // Sky: GTAO already wrote full visibility and there is no surface to filter along.
    // Blurring here would drag occlusion from the silhouette out into the sky.
    if (d_centre <= 0.0) {
        textureStore(ao_out, px, vec4<f32>(1.0));
        return;
    }
    let n_centre = oct_decode(textureLoad(normal_tex, px, 0).xy);

    let axis = select(vec2<i32>(1, 0), vec2<i32>(0, 1), params.tuning.x > 0.5);
    let radius = i32(max(params.tuning.y, 1.0));
    let depth_scale = max(params.tuning.z, 1e-4);
    let normal_power = max(params.tuning.w, 1.0);

    var sum = 0.0;
    var weight_sum = 0.0;
    for (var i = -radius; i <= radius; i = i + 1) {
        let q = clamp(px + axis * i, vec2<i32>(0), dims - vec2<i32>(1));
        let dq = textureLoad(depth_tex, q, 0);
        if (dq <= 0.0) {
            continue; // never pull sky into a surface's AO
        }

        // Gaussian-ish spatial falloff over the tap window.
        let x = f32(i) / f32(radius);
        let w_spatial = exp(-3.0 * x * x);

        // Reversed-Z depths are not linear, so compare them as a RELATIVE difference rather
        // than an absolute one. An absolute epsilon that behaves near the camera rejects
        // nothing in the distance, where reversed-Z values crowd together.
        let rel = abs(dq - d_centre) / max(max(dq, d_centre), 1e-6);
        let w_depth = exp(-rel / depth_scale);

        let nq = oct_decode(textureLoad(normal_tex, q, 0).xy);
        let w_normal = pow(clamp(dot(nq, n_centre), 0.0, 1.0), normal_power);

        let w = w_spatial * w_depth * w_normal;
        sum = sum + textureLoad(ao_in, q, 0).r * w;
        weight_sum = weight_sum + w;
    }

    // Every neighbour rejected (a one-pixel island between silhouettes) — keep the centre
    // rather than dividing by zero.
    let ao = select(textureLoad(ao_in, px, 0).r, sum / weight_sum, weight_sum > 1e-5);
    textureStore(ao_out, px, vec4<f32>(ao));
}
