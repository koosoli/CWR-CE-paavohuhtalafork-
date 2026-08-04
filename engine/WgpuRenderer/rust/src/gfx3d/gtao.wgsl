// Ground-truth ambient occlusion (docs/screen-space-ao-plan.md §3), Stages 1-3:
// scalar AO + bent normal, marched over a linear-view-Z mip chain.
//
// Inputs are what the depth+normal prepass already produces — nearest-resolved depth (via the
// mip chain built from it, gtao_depth_mips.wgsl) and a single-sample oct-encoded VIEW-space
// normal. No geometry pass of its own.
//
// The governing constraint is that this project runs MSAA and has NO TAA (plan §0), so the whole
// denoise budget is spatial. That drives two decisions visible below: the slice set is rotated by
// per-pixel interleaved gradient noise with NO frame term (a temporal term needs a temporal
// filter to resolve, and there is none), and the sample budget has to be enough on its own
// because a bilateral blur — not history — is what removes the dither.

struct GtaoParams {
    // x = proj[0][0], y = proj[1][1] (the projection's scale terms), z = camera near plane,
    // w = highest available mip index in the depth chain.
    //
    // No inverse-projection matrix. With a perspective projection and a LINEAR view z, the view
    // position is just (ndc.x / proj_xx, ndc.y / proj_yy, 1) * z — a couple of divides instead of
    // a mat4 multiply per tap. The mip chain stores linear z precisely so this is possible; see
    // gtao_depth_mips.wgsl for why reversed-Z could not be reduced meaningfully.
    proj: vec4<f32>,
    // xy = render target size in pixels, zw = 1/size.
    screen: vec4<f32>,
    // x = world-space radius (m), y = strength, z = slice count, w = steps per slice.
    tuning: vec4<f32>,
    // x = max screen radius in pixels (a sanity bound only — see px_radius below), y = thickness
    // falloff, z/w unused.
    limits: vec4<f32>,
};

@group(0) @binding(0) var depth_mips: texture_2d<f32>;
@group(0) @binding(1) var normal_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: GtaoParams;
// rgb = bent normal (VIEW space, unnormalised sum — the blur filters it and consumers
// normalise), a = ambient visibility in [0,1]. One target rather than two so the blur filters
// both with identical weights: a bent normal denoised differently from the AO it belongs to
// would disagree with it exactly at the edges where both matter.
@group(0) @binding(3) var ao_out: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;
// Matches SKY_Z in gtao_depth_mips.wgsl. Anything at or beyond this is "nothing was drawn".
const SKY_Z: f32 = 1.0e7;

// Cigolle et al. octahedral decode — must match shaders/gbuffer.wgsl's oct_encode, which is what
// the prepass wrote. Duplicated rather than imported: this module is built with include_str! and
// has no naga_oil composer, the same reason underwater.wgsl duplicates the FFT helpers.
fn oct_decode(e: vec2<f32>) -> vec3<f32> {
    var v = vec3<f32>(e.xy, 1.0 - abs(e.x) - abs(e.y));
    if (v.z < 0.0) {
        let s = vec2<f32>(select(-1.0, 1.0, v.x >= 0.0), select(-1.0, 1.0, v.y >= 0.0));
        v = vec3<f32>((vec2<f32>(1.0) - abs(v.yx)) * s, v.z);
    }
    return normalize(v);
}

// VIEW-space position for a pixel centre at a given LINEAR view z.
fn view_pos(px: vec2<f32>, z: f32) -> vec3<f32> {
    let uv = (px + vec2<f32>(0.5)) * params.screen.zw;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    return vec3<f32>(ndc.x / params.proj.x, ndc.y / params.proj.y, 1.0) * z;
}

// Linear view z at a full-resolution pixel, read from mip `mip` of the chain.
//
// Stepping up a mip as the march gets further from the centre is what lets one fixed tap budget
// cover a large screen radius. Without it the radius has to be clamped in pixels, and that clamp
// silently shortens the WORLD radius by a factor that grows as the camera closes on a surface —
// so surfaces brightened as you walked toward them, which is the artifact this replaces.
fn sample_z_mip(px: vec2<i32>, mip: i32) -> f32 {
    let dims = vec2<i32>(textureDimensions(depth_mips, mip));
    let scale = i32(1u << u32(mip));
    let c = clamp(px / scale, vec2<i32>(0), dims - vec2<i32>(1));
    return textureLoad(depth_mips, c, mip).r;
}

// Same, but with a CONTINUOUS mip level: the two neighbouring mips blended by the fraction.
//
// The mip a tap wants is a function of its screen distance, which scales with camera distance —
// so a discrete `floor(log2(...))` flips level as the camera moves, the sampled depth jumps, and
// the AO pops. That reads as a faint flicker while moving and nothing at all while still, which
// is exactly what got reported after the mip march landed.
//
// There is no temporal filter here to absorb that (plan §0: MSAA, no TAA), so the discontinuity
// has to not exist rather than be smoothed away later. Blending costs one extra texture read per
// tap and buys a level function that is continuous in camera distance. The plan's own rule is
// that the temporally stable option wins even at some GPU cost; this is that trade.
fn sample_z(px: vec2<i32>, mip_f: f32) -> f32 {
    let lo = i32(floor(mip_f));
    let hi = lo + 1;
    let f = mip_f - floor(mip_f);
    let z_lo = sample_z_mip(px, lo);
    // Past the top of the chain there is no `hi` to blend toward; hold the last level.
    let z_hi = select(z_lo, sample_z_mip(px, hi), hi <= i32(max(params.proj.w, 0.0)));
    return mix(z_lo, z_hi, f);
}

// Interleaved gradient noise (Jimenez). Spatial only — see the header note on why there is
// deliberately no frame term.
fn ign(px: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(px, vec2<f32>(0.06711056, 0.00583715))));
}

// GTAO's inner integral over a slice, from the view direction (in-plane angle 0) out to the
// horizon at angle h, with the projected normal at angle g:
//
//   F(h) = integral_0^h cos(theta - g) * sin(theta) dtheta
//        = 1/4 * (cos g - cos(2h - g) + 2h sin g)
//
// (Jimenez et al., "Practical Real-Time Strategies for Accurate Indirect Occlusion".) The cosine
// factor is the Lambert weight and sin(theta) is the Jacobian of the slice parameterisation; the
// slice's visibility is F(h_neg) + F(h_pos), which is why the two half-arcs are summed rather
// than the arc taken as one span. Passing h < 0 gives the negative half directly — the sign works
// out because the integrand is odd in sin(theta).
fn gtao_arc(h: f32, g: f32) -> f32 {
    return 0.25 * (cos(g) - cos(2.0 * h - g) + 2.0 * h * sin(g));
}

@compute @workgroup_size(8, 8, 1)
fn cs_gtao(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<i32>(textureDimensions(ao_out));
    let px = vec2<i32>(gid.xy);
    if (px.x >= dims.x || px.y >= dims.y) {
        return;
    }

    let z = sample_z_mip(px, 0);
    // Nothing drawn here. Unoccluded, and marching would integrate garbage horizons against a
    // surface that does not exist. Bent normal points at the eye so a consumer that reads it
    // anyway gets something sane.
    if (z >= SKY_Z * 0.5) {
        textureStore(ao_out, px, vec4<f32>(0.0, 0.0, -1.0, 1.0));
        return;
    }

    let pxf = vec2<f32>(px);
    let p = view_pos(pxf, z);
    let n = oct_decode(textureLoad(normal_tex, px, 0).xy);
    // View vector: view-space positions put the eye at the origin.
    let v = normalize(-p);

    let radius = max(params.tuning.x, 0.01);
    let slices = max(i32(params.tuning.z), 1);
    let steps = max(i32(params.tuning.w), 1);
    let thickness = max(params.limits.y, 0.01);
    let max_mip = i32(max(params.proj.w, 0.0));

    // Project the world radius to screen pixels at this depth. Doing it per pixel is what makes
    // the radius world-space and therefore scale-stable: a crate keeps the same contact shadow
    // near and far, instead of AO that swells as you approach.
    //
    // The upper bound is a sanity limit now, not a quality knob. Coverage no longer costs taps —
    // the march climbs the mip chain instead — so it can sit far above anything the projection
    // will ask for rather than quietly truncating the radius.
    let dist = max(length(p), 1e-3);
    let px_radius = clamp(
        radius / dist * params.proj.y * params.screen.y * 0.5,
        2.0,
        max(params.limits.x, 2.0),
    );

    let noise = ign(pxf);
    var visibility = 0.0;
    var bent = vec3<f32>(0.0);

    for (var s = 0; s < slices; s = s + 1) {
        // Rotate the whole slice set per pixel; the blur turns this dither into smooth AO.
        let phi = (f32(s) + noise) * PI / f32(slices);
        let dir = vec2<f32>(cos(phi), sin(phi));

        // The slice plane contains the eye and the screen-space line through this pixel along
        // `dir`, so EVERY point on that line's view rays lies in it. Offsetting the pixel while
        // holding z therefore lands a second in-plane point, and the direction to it is the
        // slice's in-plane axis. Reconstructing it this way rather than from a camera basis keeps
        // it exact under any projection.
        let slice_ref = view_pos(pxf + dir * max(px_radius, 1.0), z) - p;
        let w_raw = slice_ref - v * dot(slice_ref, v);
        let w_len = length(w_raw);
        if (w_len < 1e-6) {
            continue;
        }
        // In-plane orthonormal basis (v, w); w points to the +dir side of the slice.
        let w = w_raw / w_len;

        // The normal projected into the slice plane. Its LENGTH is this slice's weight (a normal
        // nearly perpendicular to the slice contributes almost nothing to it) and its ANGLE is
        // where the visible hemisphere sits. Both are per-slice: using n.v as a single global
        // factor instead — the obvious shortcut — darkens flat unoccluded ground by cos(angle),
        // so terrain fades out as you look along it. This is the whole reason GTAO carries gamma
        // through the integral rather than scaling the result.
        let n_v = dot(n, v);
        let n_w = dot(n, w);
        let proj_len = length(vec2<f32>(n_v, n_w));
        if (proj_len < 1e-6) {
            continue;
        }
        let gamma = atan2(n_w, n_v);

        // Horizon angles either side of the slice, as cosines against the view vector.
        var h1 = -1.0;
        var h2 = -1.0;
        for (var t = 1; t <= steps; t = t + 1) {
            // Offset the first tap by the same noise so neighbouring pixels do not all sample the
            // identical ring, which is what produces visible banding without a temporal term.
            let step_px = (f32(t) - 1.0 + noise) / f32(steps) * px_radius;
            let o = vec2<i32>(dir * step_px);
            // One mip per doubling of the step distance, so the footprint sampled stays roughly
            // the gap between taps and the march cannot step over an occluder it never looked at.
            // Kept FRACTIONAL and blended (see sample_z) — rounding it here is what makes AO pop
            // as the camera moves.
            let mip = clamp(log2(max(step_px, 1.0)) - 1.0, 0.0, f32(max_mip));

            let q1 = clamp(px + o, vec2<i32>(0), dims - vec2<i32>(1));
            let z1 = sample_z(q1, mip);
            if (z1 < SKY_Z * 0.5) {
                let sp = view_pos(vec2<f32>(q1), z1) - p;
                let len = length(sp);
                if (len > 1e-4) {
                    let cosh = dot(sp / len, v);
                    // Thickness heuristic: fade a sample's contribution as it recedes past the
                    // radius rather than cutting it off. Without this a thin foreground object
                    // occludes everything behind it out to infinity — GTAO's classic "sky behind
                    // a thin pole goes black".
                    let fade = clamp(1.0 - (len - radius) / thickness, 0.0, 1.0);
                    h1 = max(h1, cosh * fade);
                }
            }

            let q2 = clamp(px - o, vec2<i32>(0), dims - vec2<i32>(1));
            let z2 = sample_z(q2, mip);
            if (z2 < SKY_Z * 0.5) {
                let sp = view_pos(vec2<f32>(q2), z2) - p;
                let len = length(sp);
                if (len > 1e-4) {
                    let cosh = dot(sp / len, v);
                    let fade = clamp(1.0 - (len - radius) / thickness, 0.0, 1.0);
                    h2 = max(h2, cosh * fade);
                }
            }
        }

        // acos of a cosine horizon gives the horizon ANGLE from the view direction. The +dir side
        // is the +w half-plane (positive angles), the -dir side the -w half (negative).
        // Unoccluded stays at h = -1 -> angle PI, which the hemisphere clamp below pulls back to
        // the tangent plane, so "nothing found" and "fully open" agree.
        let a_pos = acos(clamp(h1, -1.0, 1.0));
        let a_neg = -acos(clamp(h2, -1.0, 1.0));
        // Clamp both horizons into the normal's own hemisphere, [gamma - PI/2, gamma + PI/2].
        // Without this a horizon behind the surface would contribute negative visibility.
        let hp = gamma + min(a_pos - gamma, 0.5 * PI);
        let hn = gamma + max(a_neg - gamma, -0.5 * PI);
        visibility = visibility + proj_len * (gtao_arc(hn, gamma) + gtao_arc(hp, gamma));

        // Bent normal (Stage 2): the average UNOCCLUDED direction. The midpoint of the visible
        // arc is that slice's opinion of "where the light gets in", carried back into 3D through
        // the same in-plane basis and weighted by the same proj_len as the visibility, so the two
        // stay consistent. Summed across slices and normalised at the end.
        let bent_angle = (hn + hp) * 0.5;
        bent = bent + proj_len * (v * cos(bent_angle) + w * sin(bent_angle));
    }

    // Per-slice weights (proj_len) already normalise the estimator: averaged over a full set of
    // azimuths the unoccluded case integrates to 1 for any normal, which is exactly the property
    // a global n.v factor destroys.
    visibility = visibility / f32(slices);
    // Strength as an exponent rather than a lerp to black: it deepens contact darkening without
    // flattening the midtones, and cannot push AO below 0.
    let ao = pow(clamp(visibility, 0.0, 1.0), max(params.tuning.y, 0.0));

    // Degenerate slice sets (every slice skipped) leave a zero sum; fall back to the geometric
    // normal so the ambient lookup never samples a zero direction.
    let bent_len = length(bent);
    let bent_n = select(n, bent / bent_len, bent_len > 1e-5);
    textureStore(ao_out, px, vec4<f32>(bent_n, ao));
}
