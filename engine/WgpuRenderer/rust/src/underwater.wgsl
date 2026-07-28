// cam_above is the camera's height above the local water surface in metres (negative = the eye is
// submerged). inv_view_proj unprojects a forward-NDC point to a CAMERA-RELATIVE world position,
// exactly as Frame.inv_view_proj does — it is inverted view-and-proj-separately in f64 on the Rust
// side, because the reversed-Z infinite-far projection is ill-conditioned to invert in f32.
struct Params {
    time_height_range_ext: vec4<f32>,
    camera_pos_layers: vec4<f32>,
    sun_dir_debug: vec4<f32>,
    sun_radiance: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
    shallow_color: vec4<f32>,
    deep_color: vec4<f32>,
    cascade_lengths: vec4<f32>,
    water_controls: vec4<f32>,
};

// Cleared reversed-Z depth has no finite target. This is far enough for the water volume to
// converge without letting an infinite path turn the screen into one flat colour.
const CAUSTIC_STRENGTH: f32 = 0.16;

fn srgb_to_linear_v3(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn safe_normalize(v: vec3<f32>) -> vec3<f32> {
    return v / max(length(v), 1e-5);
}

// Length of the target ray that is actually in water. This supports both sides of the
// surface: an above-water eye starts extinction only after a downward ray enters water;
// a submerged eye looking upward stops extinction where the ray exits the surface.
fn water_path_length(ray_dir: vec3<f32>, target_distance: f32, cam_above: f32) -> f32 {
    if (cam_above >= 0.0) {
        if (ray_dir.y >= -1e-4) {
            return 0.0;
        }
        let entry_distance = cam_above / max(-ray_dir.y, 1e-4);
        return max(target_distance - entry_distance, 0.0);
    }
    if (ray_dir.y > 1e-4) {
        let exit_distance = -cam_above / ray_dir.y;
        return min(target_distance, max(exit_distance, 0.0));
    }
    return target_distance;
}
@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_samp: sampler;
@group(0) @binding(2) var scene_depth: texture_depth_2d;
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var underwater_froxel: texture_3d<f32>;
@group(0) @binding(5) var caustic_tex: texture_2d<f32>;

struct VsOut { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var out: VsOut;
    out.uv = uv;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = textureDimensions(scene_depth);
    let dims_i = vec2<i32>(dims);
    let dims_f = vec2<f32>(dims);
    let pixel = clamp(vec2<i32>(in.clip.xy), vec2<i32>(0), dims_i - vec2<i32>(1));
    // Reversed-Z depth is proportional to 1/distance. The approximation deliberately
    // saturates for sky/cleared depth, producing dense distant underwater haze.
    let base_depth = textureLoad(scene_depth, pixel, 0);
    // Two broad travelling waves refract the completed scene beneath the surface.
    // Keep this in screen space and below three pixels so it reads as water volume,
    // not a camera shake.
    let wave = vec2<f32>(
        sin(in.uv.y * 17.0 + params.time_height_range_ext.x * 1.10) +
            sin(in.uv.x * 9.0 - params.time_height_range_ext.x * 0.63),
        sin(in.uv.x * 15.0 - params.time_height_range_ext.x * 0.92) +
            sin(in.uv.y * 7.0 + params.time_height_range_ext.x * 0.48)
    ) * 0.5;
    let warp_limit = 3.0 / dims_f;
    let warped_uv = clamp(in.uv + wave * warp_limit, vec2<f32>(0.001), vec2<f32>(0.999));
    let warped_pixel = clamp(vec2<i32>(warped_uv * dims_f), vec2<i32>(0), dims_i - vec2<i32>(1));
    let warped_depth = textureLoad(scene_depth, warped_pixel, 0);
    // Do not refract a closer foreground object over its background neighbour.
    let use_warp = warped_depth <= base_depth + 0.001;
    let sample_uv = select(in.uv, warped_uv, use_warp);
    let depth = select(base_depth, warped_depth, use_warp);

    // Reconstruct a world-oriented camera ray even when the opaque depth is cleared. For finite
    // geometry the exact unprojected position supplies its metric distance.
    let ndc_xy = vec2<f32>(sample_uv.x * 2.0 - 1.0, 1.0 - sample_uv.y * 2.0);
    let ray_h = params.inv_view_proj * vec4<f32>(ndc_xy, 0.5, 1.0);
    let ray_point = ray_h.xyz / max(abs(ray_h.w), 1e-5) * sign(ray_h.w);
    var ray_dir = safe_normalize(ray_point);
    let max_path_m = params.time_height_range_ext.z;
    var target_distance = max_path_m;
    var world_rel = ray_dir * target_distance;
    if (depth > 1e-6) {
        let h = params.inv_view_proj * vec4<f32>(ndc_xy, 1.0 - depth, 1.0);
        world_rel = h.xyz / max(abs(h.w), 1e-5) * sign(h.w);
        target_distance = clamp(length(world_rel), 0.0, max_path_m);
        ray_dir = safe_normalize(world_rel);
    }

    let cam_above = params.time_height_range_ext.y;
    let path_m = water_path_length(ray_dir, target_distance, cam_above);
    if (path_m <= 1e-4) {
        // The compositor runs in a small band above the surface for split waterline views.
        // Rays that never enter water must remain completely untouched, including refraction.
        return vec4<f32>(textureSampleLevel(scene_tex, scene_samp, in.uv, 0.0).rgb, 1.0);
    }
    let color = textureSampleLevel(scene_tex, scene_samp, sample_uv, 0.0).rgb;

    // Use the same physically plausible RGB absorption curve as the surface shader. The previous
    // rewrite used pow(deep_colour, path*extinction); the authored deep swatch is very dark, so
    // that destroyed almost all transmission within a few metres and left only uniform blue haze.
    let ext = max(params.time_height_range_ext.w, 1e-3);
    let extinction_rgb = vec3<f32>(0.280, 0.065, 0.020) * max(ext * 2.5, 0.12);
    let transmittance = exp(-extinction_rgb * path_m);

    // The frustum-aligned volume carries integrated, terrain/object-shadowed in-scattering.
    // A single trilinear sample replaces the former uniform blue haze.
    let froxel_w = sqrt(clamp(target_distance / max(max_path_m, 1e-3), 0.0, 1.0));
    let volume = textureSampleLevel(
        underwater_froxel,
        scene_samp,
        vec3<f32>(in.uv, froxel_w),
        0.0
    );

    // FFT compression and curvature generate a camera-centred world-space caustic field.
    let world_xz = params.camera_pos_layers.xz + world_rel.xz;
    let caustic_uv = (world_xz - params.camera_pos_layers.xz) / 256.0 + 0.5;
    let caustic_pattern = textureSampleLevel(
        caustic_tex,
        scene_samp,
        clamp(caustic_uv, vec2<f32>(0.0), vec2<f32>(1.0)),
        0.0
    ).r;
    let geometry_mask = select(0.0, 1.0, depth > 1e-6);
    let surface_light = exp(-max(-cam_above, 0.0) * 0.12);
    let caustic = 1.0 + CAUSTIC_STRENGTH * caustic_pattern *
        exp(-path_m * 0.055) * geometry_mask * surface_light;

    // Looking up from shallow water retains a soft bright surface veil rather than turning the
    // sky into the same blue fog as the seabed. This is a single analytic term, not a ray march.
    let surface_veil = vec3<f32>(0.010, 0.035, 0.042) *
        pow(clamp(ray_dir.y, 0.0, 1.0), 3.0) * surface_light;
    let debug_view = i32(params.sun_dir_debug.w);
    if (debug_view == 30) {
        return vec4<f32>(transmittance, 1.0);
    }
    if (debug_view == 31) {
        return vec4<f32>(volume.rgb * 8.0, 1.0);
    }
    if (debug_view == 32) {
        return vec4<f32>(vec3<f32>(volume.a), 1.0);
    }
    if (debug_view == 33) {
        return vec4<f32>(vec3<f32>(caustic_pattern), 1.0);
    }
    let result = color * transmittance * caustic + volume.rgb + surface_veil;
    return vec4<f32>(result, 1.0);
}
