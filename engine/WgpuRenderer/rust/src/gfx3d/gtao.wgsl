// Ground-truth ambient occlusion (docs/screen-space-ao-plan.md §3), Stage 1: scalar AO.
//
// Inputs are exactly what the depth+normal prepass already produces — single-sample
// nearest-resolved depth and a single-sample oct-encoded view-space normal. No geometry
// pass of its own.
//
// The governing constraint is that this project runs MSAA and has NO TAA (plan §0), so the
// whole denoise budget is spatial. That drives two decisions visible below: the slice set is
// rotated by per-pixel interleaved gradient noise with NO frame term (a temporal term would
// need a temporal filter to resolve, and there is none), and the sample budget has to be
// enough on its own because a bilateral blur — not history — is what removes the dither.

struct GtaoParams {
    // inv(view) * inv(proj), matching Frame.inv_view_proj: unprojects an NDC point to a
    // CAMERA-RELATIVE world position.
    inv_view_proj: mat4x4<f32>,
    // xy = render target size in pixels, zw = 1/size.
    screen: vec4<f32>,
    // x = world-space radius (m), y = strength, z = slice count, w = steps per slice.
    tuning: vec4<f32>,
    // x = max screen radius in pixels (cost clamp), y = thickness/falloff heuristic,
    // z = camera near plane, w unused.
    limits: vec4<f32>,
};

@group(0) @binding(0) var depth_tex: texture_depth_2d;
@group(0) @binding(1) var normal_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: GtaoParams;
@group(0) @binding(3) var ao_out: texture_storage_2d<r8unorm, write>;

const PI: f32 = 3.14159265;

// Cigolle et al. octahedral decode — must match shaders/gbuffer.wgsl's oct_encode, which is
// what the prepass wrote. Duplicated rather than imported: this module is built with
// include_str! and has no naga_oil composer, the same reason underwater.wgsl duplicates the
// FFT sampling helpers.
fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    var v = vec3<f32>(e.xy, 1.0 - abs(e.x) - abs(e.y));
    if (v.z < 0.0) {
        let s = vec2<f32>(select(-1.0, 1.0, v.x >= 0.0), select(-1.0, 1.0, v.y >= 0.0));
        v = vec3<f32>((vec2<f32>(1.0) - abs(v.yx)) * s, v.z);
    }
    return normalize(v);
}

// Camera-relative world position for a pixel centre at the given reversed-Z depth.
// Returns a zero-length vector when depth is cleared (nothing was drawn there).
fn view_pos(px: vec2<i32>, d: f32) -> vec3<f32> {
    let uv = (vec2<f32>(px) + vec2<f32>(0.5)) * params.screen.zw;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    // Reversed-Z: the depth value goes straight into the unprojection as written.
    let h = params.inv_view_proj * vec4<f32>(ndc, d, 1.0);
    return h.xyz / max(abs(h.w), 1e-6) * sign(h.w);
}

// Interleaved gradient noise (Jimenez). Spatial only — see the header note on why there is
// deliberately no frame term.
fn ign(px: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(px, vec2<f32>(0.06711056, 0.00583715))));
}

@compute @workgroup_size(8, 8, 1)
fn cs_gtao(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(ao_out));
    let px = vec2<i32>(gid.xy);
    if (px.x >= dims.x || px.y >= dims.y) {
        return;
    }

    let d = textureLoad(depth_tex, px, 0);
    // Cleared reversed-Z depth is 0 — sky. Unoccluded, and marching there would integrate
    // garbage horizons against a surface that does not exist.
    if (d <= 0.0) {
        textureStore(ao_out, px, vec4<f32>(1.0));
        return;
    }

    let p = view_pos(px, d);
    let n = oct_decode(textureLoad(normal_tex, px, 0).xy);
    // View vector: positions are camera-relative, so the eye is the origin.
    let v = normalize(-p);

    let radius = max(params.tuning.x, 0.01);
    let slices = max(i32(params.tuning.z), 1);
    let steps = max(i32(params.tuning.w), 1);
    let thickness = max(params.limits.y, 0.01);

    // Project the world radius to screen pixels at this depth, then clamp for cost. Doing it
    // per pixel is what makes the radius world-space and therefore scale-stable: a crate has
    // the same contact shadow near and far, instead of AO that swells as you approach.
    let dist = max(length(p), 1e-3);
    let px_radius = clamp(radius / dist * params.screen.y * 0.5, 2.0, max(params.limits.x, 2.0));

    let noise = ign(vec2<f32>(px));
    var visibility = 0.0;

    for (var s = 0; s < slices; s = s + 1) {
        // Rotate the whole slice set per pixel; the blur turns this dither into smooth AO.
        let phi = (f32(s) + noise) * PI / f32(slices);
        let dir = vec2<f32>(cos(phi), sin(phi));

        // Horizon angles either side of the slice, as cosines against the view vector.
        var h1 = -1.0;
        var h2 = -1.0;
        for (var t = 1; t <= steps; t = t + 1) {
            // Offset the first tap by the same noise so neighbouring pixels do not all sample
            // the identical ring, which is what produces visible banding without a temporal term.
            let step_px = (f32(t) - 1.0 + noise) / f32(steps) * px_radius;
            let o = vec2<i32>(dir * step_px);

            let q1 = clamp(px + o, vec2<i32>(0), dims - vec2<i32>(1));
            let d1 = textureLoad(depth_tex, q1, 0);
            if (d1 > 0.0) {
                let sp = view_pos(q1, d1) - p;
                let len = length(sp);
                if (len > 1e-4) {
                    let cosh = dot(sp / len, v);
                    // Thickness heuristic: fade a sample's contribution as it recedes past the
                    // radius rather than cutting it off. Without this a thin foreground object
                    // occludes everything behind it out to infinity — GTAO's classic
                    // "sky behind a thin pole goes black".
                    let fade = clamp(1.0 - (len - radius) / thickness, 0.0, 1.0);
                    h1 = max(h1, cosh * fade);
                }
            }

            let q2 = clamp(px - o, vec2<i32>(0), dims - vec2<i32>(1));
            let d2 = textureLoad(depth_tex, q2, 0);
            if (d2 > 0.0) {
                let sp = view_pos(q2, d2) - p;
                let len = length(sp);
                if (len > 1e-4) {
                    let cosh = dot(sp / len, v);
                    let fade = clamp(1.0 - (len - radius) / thickness, 0.0, 1.0);
                    h2 = max(h2, cosh * fade);
                }
            }
        }

        // Arc between the two horizons, projected onto the pixel normal. acos of a cosine
        // horizon gives the horizon ANGLE; the visible wedge is what is left between them.
        let a1 = acos(clamp(h1, -1.0, 1.0));
        let a2 = acos(clamp(h2, -1.0, 1.0));
        let n_dot_v = clamp(dot(n, v), 0.0, 1.0);
        // Cosine-weighted visible fraction of the slice, normalised so a fully open slice
        // integrates to 1.
        visibility = visibility + clamp((a1 + a2) / PI, 0.0, 1.0) * n_dot_v;
    }

    visibility = visibility / f32(slices);
    // Strength as an exponent rather than a lerp to black: it deepens contact darkening
    // without flattening the midtones, and cannot push AO below 0.
    let ao = pow(clamp(visibility, 0.0, 1.0), max(params.tuning.y, 0.0));
    textureStore(ao_out, px, vec4<f32>(ao));
}
