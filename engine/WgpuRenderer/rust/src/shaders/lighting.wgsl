#define_import_path lighting

#import frame::{frame, lights}
#import color::srgb_to_linear

// Sun lighting matching GL33's lit path: diffuse * N.L + ambient (eye
// accommodation folded in on the CPU), saturated like the vertex-colour pack it
// replaces. sun_dir_world is the light's travel direction (GL33's sunDir
// constant, negated against the true up normal exactly as GL33's vertex shader
// does); at night/dawn it points at or up through the horizon, so level ground
// falls back to ambient. Not the shadow block's sun_dir, which is only valid
// while the cascade pass runs.
//
// `linear` (the HDR path's pipeline override, 0/1): decode the CPU-supplied
// gamma-space sun colours to linear and drop the [0,1] saturate so radiance can
// exceed 1.0 into the HDR target. 0 = the exact gamma-naive GL33 behaviour.
fn sun_light(normal_ws: vec3<f32>, linear: f32) -> vec3<f32> {
    let cos_fi = max(dot(normal_ws, -frame.sun_dir_world.xyz), 0.0);
    var diffuse = frame.sun_diffuse.rgb;
    var ambient = frame.sun_ambient.rgb;
    if (linear > 0.5) {
        diffuse = srgb_to_linear(diffuse);
        ambient = srgb_to_linear(ambient);
        return diffuse * cos_fi + ambient;
    }
    return min(diffuse * cos_fi + ambient, vec3<f32>(1.0));
}

// Cone falloff band for spotlights: full inside cos(8deg), zero outside cos(12deg)
// (GL33's LightReflector cone), interpolated in cos^2 of the angle from the axis.
const LOCAL_MIN_INSIDE2: f32 = 0.95677279; // (cos 12deg)^2
const LOCAL_MAX_INSIDE2: f32 = 0.98063081; // (cos 8deg)^2

// Accumulated point/spot light contribution at a fragment, matching GL33's
// VSNormal local-light loop but per-fragment. `world_rel` is the camera-relative
// fragment position both entry shaders already carry; the light's camera-relative
// position is reconstructed from its absolute position via frame.cam_pos.
// mat_diffuse/mat_ambient modulate the per-light colour (GL33's matDif/matAmb;
// pass white for terrain). Quadratic falloff past the light's start distance, cut
// off at 100x; the light colours already fold in NightEffect, so day = no-op.
fn lights_contrib(world_rel: vec3<f32>, normal_ws: vec3<f32>, mat_diffuse: vec3<f32>, mat_ambient: vec3<f32>, linear: f32) -> vec3<f32> {
    var acc = vec3<f32>(0.0);
    let count = u32(max(frame.cam_pos.w, 0.0));
    for (var i = 0u; i < count; i = i + 1u) {
        let light = lights[i];
        // Per-light gamma-space colours decoded to linear for the HDR path (the
        // caller passes already-linearized mat_diffuse/mat_ambient). The night
        // dimming factor folded into these on the CPU stays as a linear scalar.
        var l_diffuse = light.diffuse.rgb;
        var l_ambient = light.ambient.rgb;
        if (linear > 0.5) {
            l_diffuse = srgb_to_linear(l_diffuse);
            l_ambient = srgb_to_linear(l_ambient);
        }
        // Camera-relative light position, then the fragment-to-light vector.
        let to_light = (light.pos.xyz - frame.cam_pos.xyz) - world_rel;
        let size2 = dot(to_light, to_light);
        let start_atten2 = light.pos.w * light.pos.w;
        let end_atten2 = start_atten2 * 100.0;
        if (size2 >= end_atten2) {
            continue;
        }
        var cone = 1.0;
        if (light.dir.w > 0.5) {
            // inside = (light -> fragment) . beam axis; cos^2 = inside^2 / size2.
            let inside = -dot(to_light, light.dir.xyz);
            if (inside <= 0.0) {
                continue;
            }
            let cos2 = (inside * inside) / size2;
            if (cos2 < LOCAL_MIN_INSIDE2) {
                continue;
            }
            cone = clamp((cos2 - LOCAL_MIN_INSIDE2) / (LOCAL_MAX_INSIDE2 - LOCAL_MIN_INSIDE2), 0.0, 1.0);
        }
        // GL33's flat-core / inverse-square attenuation, but with the tail smoothly
        // windowed to zero at end_atten2. GL33 cuts the tail hard — invisible in its
        // clamped LDR output, but the HDR exposure turns that discontinuity into a
        // visible ring. The window is ~1 across the near field (it only bites as the
        // fragment approaches the cutoff), so it doesn't change GL33's near look.
        let base_atten = select(1.0, start_atten2 / size2, size2 >= start_atten2);
        let fade = clamp(1.0 - (size2 / end_atten2) * (size2 / end_atten2), 0.0, 1.0);
        let atten = base_atten * fade * fade;
        let cos_fi = dot(to_light, normal_ws);
        if (cos_fi > 0.0) {
            let ndotl = cos_fi * inverseSqrt(size2);
            acc += (l_diffuse * mat_diffuse * ndotl + l_ambient * mat_ambient) * (atten * cone);
        } else {
            // Back-facing: ambient only, no cone gate (GL33 parity).
            acc += l_ambient * mat_ambient * atten;
        }
    }
    return acc;
}
