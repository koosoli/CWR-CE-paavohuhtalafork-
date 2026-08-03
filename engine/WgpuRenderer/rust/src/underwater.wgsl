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

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_samp: sampler;
@group(0) @binding(2) var scene_depth: texture_depth_2d;
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var underwater_froxel: texture_3d<f32>;
@group(0) @binding(5) var caustic_tex: texture_2d<f32>;
@group(0) @binding(6) var fft_displacement: texture_2d_array<f32>;

// --- Wavy surface lookup -------------------------------------------------------------
// Duplicated from water_fft_sampling, as underwater_caustics.wgsl already duplicates it:
// this module is built with include_str!, not the naga_oil composer the water shaders use,
// so it cannot #import. Keep the three functions below byte-identical in behaviour to
// water/fft_sampling.wgsl or the compositor's waterline will sit somewhere the drawn
// surface is not.
fn fft_hash(cell: vec2<i32>) -> f32 {
    let c = vec2<u32>(cell) & vec2<u32>(0xffffu);
    var n = c.x * 1597334677u + c.y * 3812015801u;
    n = (n ^ (n >> 15u)) * 2246822519u;
    n = n ^ (n >> 13u);
    return f32(n & 0x00ffffffu) / 16777216.0;
}

fn fft_value_noise(p: vec2<f32>) -> f32 {
    let cell = vec2<i32>(floor(p));
    let f = fract(p);
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = fft_hash(cell);
    let b = fft_hash(cell + vec2<i32>(1, 0));
    let c = fft_hash(cell + vec2<i32>(0, 1));
    let d = fft_hash(cell + vec2<i32>(1, 1));
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

fn fft_aperiodic_uv(world_xz: vec2<f32>, tile_length: f32, layer: i32, amplitude: f32) -> vec2<f32> {
    if (amplitude <= 0.0) {
        return fract(world_xz / max(tile_length, 1.0));
    }
    let l = f32(layer);
    let broad_p = world_xz * 0.00173 + vec2<f32>(l * 13.7, l * -19.1);
    let fine_p = world_xz * 0.00891 + vec2<f32>(l * -7.3, l * 11.9);
    let broad = vec2<f32>(
        fft_value_noise(broad_p),
        fft_value_noise(broad_p + vec2<f32>(41.3, 17.9))
    ) * 2.0 - vec2<f32>(1.0);
    let fine = vec2<f32>(
        fft_value_noise(fine_p),
        fft_value_noise(fine_p + vec2<f32>(23.1, 37.7))
    ) * 2.0 - vec2<f32>(1.0);
    let warped = world_xz + (broad * 0.72 + fine * 0.28) * max(amplitude, 0.0) *
        (1.0 + l * 0.17);
    let angle = 0.173 * l + 0.071;
    let ca = cos(angle);
    let sa = sin(angle);
    let rotated = vec2<f32>(
        ca * warped.x - sa * warped.y,
        sa * warped.x + ca * warped.y
    );
    return fract(rotated / max(tile_length, 1.0) + vec2<f32>(0.173 * l, 0.347 * l));
}

// Vertical wave displacement above `sea_level` at a world XZ, summed over the cascades
// that actually move geometry.
//
// This is the near-field case of water.wgsl's fft_geometry_disp, and deliberately not a
// copy of it. Two terms of that function are dropped:
//
//   * the projected-footprint LOD weights, which are ~1 for every geometry cascade within
//     a few metres of the eye — and the waterline is by definition at the eye;
//   * the shoaling redistribution, which is energy preserving, so it moves height between
//     cascades rather than changing the sum.
//
// The `raw_length < 20` test is kept verbatim: those cascades are normal/foam detail and
// contribute no displacement, so including them would raise the surface above where it
// is drawn.
//
// One approximation remains: the surface is sampled at the world XZ rather than at the
// material coordinate that horizontal choppiness displaced to it. That shifts the wave
// pattern laterally by up to the choppy amplitude, which moves the waterline along the
// surface but not up or down — the eye is still judged against a crest when a crest is
// overhead.
fn surface_height_at(world_xz: vec2<f32>) -> f32 {
    let active_layers = clamp(i32(params.camera_pos_layers.w), 0, 4);
    if (active_layers <= 0) {
        return 0.0;
    }
    let warp = params.water_controls.x;
    let scale = max(params.water_controls.z, 0.01);
    let scaled_xz = world_xz / scale;
    var height = 0.0;
    for (var layer = 0; layer < 4; layer = layer + 1) {
        if (layer >= active_layers) {
            break;
        }
        let raw_length = params.cascade_lengths[layer];
        if (raw_length < 20.0) {
            continue;
        }
        let length_m = max(raw_length, 1.0);
        let uv = fft_aperiodic_uv(scaled_xz, length_m, layer, warp);
        // fft_sample scales the stored displacement by the same factor it dilates the
        // lookup by, so wave steepness stays constant. Match that here.
        height = height + textureSampleLevel(fft_displacement, scene_samp, uv, layer, 0.0).y * scale;
    }
    return height;
}

// Signed height of the ray above the wavy surface, `t` metres along it. Negative is
// underwater.
fn ray_above_surface(ray_dir: vec3<f32>, t: f32) -> f32 {
    let p_xz = params.camera_pos_layers.xz + ray_dir.xz * t;
    let p_y = params.camera_pos_layers.y + ray_dir.y * t;
    return p_y - (params.water_controls.y + surface_height_at(p_xz));
}

// How far the near field is searched for a surface crossing, and in how many steps.
// 48 m at 12 steps is a 4 m stride — comfortably under the shortest cascade that carries
// displacement (20 m), so a crest cannot hide between two samples. Beyond the probe the
// flat model takes over, which is what the whole compositor used to do at every distance.
const WATERLINE_PROBE_M: f32 = 48.0;
const WATERLINE_STEPS: i32 = 12;

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

// Wave-aware replacement for water_path_length.
//
// The flat version asks a single question — is the eye above or below one plane — and
// answers it identically for every pixel on the screen. Two things follow from that, and
// both are the bugs this function exists to remove:
//
//   * a crest passing over the eye never counts, because the plane sits at the mean level;
//   * a view straddling the surface is entirely wet or entirely dry, because one whole-
//     screen answer cannot draw a waterline.
//
// Marching the ray fixes both at once. The sign of `ray_above_surface` is evaluated along
// the ray, and the FIRST place it changes is where this pixel's ray enters or leaves the
// water. Rays that pass under a crest cross there; neighbouring rays that pass over a
// trough cross somewhere else or not at all — so the waterline appears as the boundary
// between them, at whatever shape the waves actually have.
//
// Cost is one surface_height_at per step, so it stays behind the compositor's existing
// underwater-only gate; it is not paid on a dry frame.
fn wavy_water_path_length(ray_dir: vec3<f32>, target_distance: f32, flat_above: f32) -> f32 {
    // No FFT cascades means no displacement to sample — the binding is the zero fallback
    // array. Use the CPU's whole-screen height, which on the Gerstner preset already
    // carries that preset's analytic wave height; marching a flat field would only find
    // the same answer more slowly, and would flatten that preset's waves.
    if (i32(params.camera_pos_layers.w) <= 0) {
        return water_path_length(ray_dir, target_distance, flat_above);
    }
    let probe = min(target_distance, WATERLINE_PROBE_M);
    let step = probe / f32(WATERLINE_STEPS);
    var prev_t = 0.0;
    var prev_f = ray_above_surface(ray_dir, 0.0);
    let start_submerged = prev_f < 0.0;
    var cross_t = -1.0;
    for (var i = 1; i <= WATERLINE_STEPS; i = i + 1) {
        let t = f32(i) * step;
        let f = ray_above_surface(ray_dir, t);
        if ((f < 0.0) != start_submerged) {
            // Linear root between the bracketing samples. The denominator has the sign of
            // prev_f in either crossing direction, so the fraction is positive both ways.
            let denom = prev_f - f;
            let safe_denom = select(denom, 1e-5, abs(denom) < 1e-5);
            cross_t = mix(prev_t, t, clamp(prev_f / safe_denom, 0.0, 1.0));
            break;
        }
        prev_t = t;
        prev_f = f;
    }
    if (cross_t >= 0.0) {
        // Submerged rays are in water up to where they surface; dry rays are in water from
        // where they dive to wherever the scene stopped them.
        if (start_submerged) {
            return cross_t;
        }
        return max(target_distance - cross_t, 0.0);
    }
    // Nothing crossed inside the probe. Hand the remaining distance to the flat model,
    // starting from the far end of the probe where prev_f is the ray's height above the
    // surface — beyond a few tens of metres the wave detail no longer resolves anyway.
    let tail = water_path_length(ray_dir, max(target_distance - probe, 0.0), prev_f);
    return select(tail, probe + tail, start_submerged);
}

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

    // time_height_range_ext.y is the CPU's single whole-screen height above the flat
    // datum; it survives only as the no-FFT fallback inside wavy_water_path_length.
    let cam_above = params.time_height_range_ext.y;
    let path_m = wavy_water_path_length(ray_dir, target_distance, cam_above);
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
