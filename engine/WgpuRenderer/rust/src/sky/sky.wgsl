// Procedural atmospheric sky — Hillaire-style LUT model (plan Stage 2a). One shader
// module with a shared atmosphere and three fragment entry points:
//   fs_transmittance : builds the transmittance LUT   T(r, mu_sun)     (params-only)
//   fs_multiscatter  : builds the multi-scattering LUT Psi(r, mu_sun)  (params-only)
//   fs_sky           : the fullscreen sky — marches the view ray, samples both LUTs
//                      for sun transmittance + multiscatter, adds a transmittance-
//                      attenuated sun disc, night-faded horizon haze, and outputs
//                      linear radiance (HDR) or self-tonemaps (LDR-direct).
// The LUTs depend only on atmosphere params (the sun is a LUT axis), so they rebuild
// only when those change. See docs/procedural-sky-plan.md §3 (Stage 2) and §8.

struct Sky {
    inv_view_proj: mat4x4<f32>,
    sun_dir: vec4<f32>,       // xyz = dir TO sun, w = sun radiance scale
    moon_dir: vec4<f32>,      // xyz = dir TO moon, w = moon phase (unused here)
    rayleigh: vec4<f32>,      // xyz = Rayleigh beta (1/m), w = Rayleigh scale height (m)
    mie: vec4<f32>,           // x = Mie beta, y = Mie g, z = Mie scale height (m), w = turbidity
    ground_albedo: vec4<f32>, // xyz = ground albedo, w = night factor
    params: vec4<f32>,        // x = sun angular radius (rad), y = exposure, z = planet radius (m), w = atmosphere (m)
    control: vec4<f32>,       // x = enabled, y = view samples, z = light samples, w = ozone strength
    fog_color: vec4<f32>,     // xyz = scene fog colour, w = horizon-haze strength
    night_zenith: vec4<f32>,  // xyz = night radiance at zenith, w = camera altitude ASL (m)
    night_horizon: vec4<f32>, // xyz = night radiance at horizon
    night_params: vec4<f32>,  // x = full-day sun.y, y = full-night sun.y, z = intensity, w = far-fade range (m)
    output: vec4<f32>,        // x = linear output (1) vs self-tonemap (0)
    cam_pos: vec4<f32>,       // xyz = ABSOLUTE world camera position (froxel -> terrain-mask lookup)
};

@group(0) @binding(0) var<uniform> sky: Sky;
@group(0) @binding(1) var lut_sampler: sampler;
@group(0) @binding(2) var transmittance_lut: texture_2d<f32>;
@group(0) @binding(3) var multiscatter_lut: texture_2d<f32>;

const PI: f32 = 3.14159265359;
const TRANSMITTANCE_STEPS: f32 = 40.0;
const MS_SQRT_SAMPLES: i32 = 8;        // 8x8 = 64 sphere directions
const MS_STEPS: f32 = 20.0;
// Mie extinction ~ 1.11x its scattering (a little absorption in the aerosol layer).
const MIE_EXT: f32 = 1.11;
// Ozone absorption coefficients (1/m at peak density; Earth values). Absorbs green
// and red far more than blue, which is what keeps twilight/zenith blue.
const OZONE_ABSORPTION: vec3<f32> = vec3<f32>(0.650e-6, 1.881e-6, 0.085e-6);

// Ozone density: absorption-only tent peaking mid-atmosphere, scaled to the
// atmosphere thickness (Earth: ~25 km peak, ~15 km half-width in a 60 km shell).
fn ozone_density(alt: f32) -> f32 {
    let atmos_h = sky.params.w;
    return max(0.0, 1.0 - abs(alt - atmos_h * 0.417) / (atmos_h * 0.25));
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) ray_dir: vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    let ndc = uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = uv;
    // World view ray (camera at origin — the view matrix has no translation).
    // Unproject at the NEAR plane (forward NDC z = 0), not the far plane: with an
    // infinite-far projection NDC z = 1 is the point at infinity, where w -> 0 and the
    // divide explodes into NaNs (black sky). Any finite depth on the ray gives the same
    // direction since the camera sits at the origin, so the near plane is the safe pick.
    let world = sky.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    out.ray_dir = world.xyz / world.w;
    return out;
}

// Ray/sphere intersection (sphere centred at the planet origin, radius `radius`).
// Returns near/far parametric distances; x > y means a miss.
fn ray_sphere(origin: vec3<f32>, dir: vec3<f32>, radius: f32) -> vec2<f32> {
    let b = dot(origin, dir);
    let c = dot(origin, origin) - radius * radius;
    let d = b * b - c;
    if (d < 0.0) {
        return vec2<f32>(1.0, -1.0);
    }
    let s = sqrt(d);
    return vec2<f32>(-b - s, -b + s);
}

struct Medium {
    rayleigh: vec3<f32>,   // Rayleigh scattering coeff at this altitude
    mie: f32,              // Mie scattering coeff
    extinction: vec3<f32>, // total extinction (scattering + Mie absorption)
};

fn scattering_values(pos: vec3<f32>) -> Medium {
    let planet_r = sky.params.z;
    let alt = length(pos) - planet_r;
    let rho_r = exp(-alt / sky.rayleigh.w);
    let rho_m = exp(-alt / max(sky.mie.z, 1.0));
    var m: Medium;
    m.rayleigh = sky.rayleigh.xyz * rho_r;
    m.mie = sky.mie.x * rho_m;
    // Ozone (absorption only) keeps twilight/zenith blue — without it the reddened
    // low-sun light re-scatters sickly green. control.w scales it (blue-hour knob).
    m.extinction = m.rayleigh + vec3<f32>(m.mie * MIE_EXT)
        + OZONE_ABSORPTION * ozone_density(alt) * sky.control.w;
    return m;
}

// Non-linear cos-angle <-> texture-U mapping that concentrates LUT resolution near
// the horizon (mu = 0), where transmittance changes fastest and the long grazing
// ozone path that makes the blue hour lives. A linear axis under-samples it and the
// twilight blue falls between texels. Inverse pair.
fn mu_to_u(mu: f32) -> f32 {
    return sign(mu) * sqrt(abs(mu)) * 0.5 + 0.5;
}
fn u_to_mu(u: f32) -> f32 {
    let x = u * 2.0 - 1.0;
    return sign(x) * x * x;
}

fn tlut_uv(pos: vec3<f32>, dir: vec3<f32>) -> vec2<f32> {
    let planet_r = sky.params.z;
    let top_r = planet_r + sky.params.w;
    let h = length(pos);
    let up = pos / h;
    let mu = dot(dir, up);
    return vec2<f32>(mu_to_u(mu), clamp((h - planet_r) / (top_r - planet_r), 0.0, 1.0));
}

fn sample_transmittance(pos: vec3<f32>, dir: vec3<f32>) -> vec3<f32> {
    return textureSampleLevel(transmittance_lut, lut_sampler, tlut_uv(pos, dir), 0.0).rgb;
}

fn sample_multiscatter(pos: vec3<f32>, dir: vec3<f32>) -> vec3<f32> {
    return textureSampleLevel(multiscatter_lut, lut_sampler, tlut_uv(pos, dir), 0.0).rgb;
}

fn rayleigh_phase(cos_t: f32) -> f32 {
    return 3.0 / (16.0 * PI) * (1.0 + cos_t * cos_t);
}

fn mie_phase(cos_t: f32) -> f32 {
    let g = sky.mie.y;
    let num = (1.0 - g * g) * (1.0 + cos_t * cos_t);
    let den = (2.0 + g * g) * pow(1.0 + g * g - 2.0 * g * cos_t, 1.5);
    return 3.0 / (8.0 * PI) * num / den;
}

// ---- Transmittance LUT -------------------------------------------------------

@fragment
fn fs_transmittance(in: VsOut) -> @location(0) vec4<f32> {
    let planet_r = sky.params.z;
    let top_r = planet_r + sky.params.w;
    let r = mix(planet_r, top_r, in.uv.y);
    let mu = u_to_mu(in.uv.x);
    let pos = vec3<f32>(0.0, r, 0.0);
    let dir = vec3<f32>(sqrt(max(1.0 - mu * mu, 0.0)), mu, 0.0);

    // A ray that hits the planet reaches no light: transmittance is zero (the sun
    // is below this point's local horizon — planet shadow).
    if (ray_sphere(pos, dir, planet_r).x > 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let t_max = ray_sphere(pos, dir, top_r).y;

    var od_r = 0.0;
    var od_m = 0.0;
    var od_o = 0.0;
    var t = 0.0;
    for (var i = 0.0; i < TRANSMITTANCE_STEPS; i += 1.0) {
        let new_t = ((i + 0.5) / TRANSMITTANCE_STEPS) * t_max;
        let dt = new_t - t;
        t = new_t;
        let p = pos + dir * t;
        let alt = length(p) - planet_r;
        od_r += exp(-alt / sky.rayleigh.w) * dt;
        od_m += exp(-alt / max(sky.mie.z, 1.0)) * dt;
        od_o += ozone_density(alt) * dt;
    }
    // Ozone MUST be in the transmittance LUT too: it colours the sunlight reaching
    // every scattering point, which is the dominant tint of the twilight sky.
    let tau = sky.rayleigh.xyz * od_r + vec3<f32>(sky.mie.x * MIE_EXT) * od_m
        + OZONE_ABSORPTION * od_o * sky.control.w;
    return vec4<f32>(exp(-tau), 1.0);
}

// ---- Multiple-scattering LUT (Hillaire isotropic approximation) --------------

@fragment
fn fs_multiscatter(in: VsOut) -> @location(0) vec4<f32> {
    let planet_r = sky.params.z;
    let top_r = planet_r + sky.params.w;
    let r = mix(planet_r, top_r, in.uv.y);
    let mu_s = u_to_mu(in.uv.x);
    let pos = vec3<f32>(0.0, r, 0.0);
    let sun = vec3<f32>(sqrt(max(1.0 - mu_s * mu_s, 0.0)), mu_s, 0.0);

    let inv_samples = 1.0 / f32(MS_SQRT_SAMPLES * MS_SQRT_SAMPLES);
    var lum_total = vec3<f32>(0.0);
    var fms_total = vec3<f32>(0.0);

    for (var i = 0; i < MS_SQRT_SAMPLES; i = i + 1) {
        for (var j = 0; j < MS_SQRT_SAMPLES; j = j + 1) {
            // Uniform sphere sample.
            let u = (f32(i) + 0.5) / f32(MS_SQRT_SAMPLES);
            let v = (f32(j) + 0.5) / f32(MS_SQRT_SAMPLES);
            let cos_th = 1.0 - 2.0 * u;
            let sin_th = sqrt(max(0.0, 1.0 - cos_th * cos_th));
            let phi = 2.0 * PI * v;
            let ray = vec3<f32>(sin_th * cos(phi), cos_th, sin_th * sin(phi));

            var t_max = ray_sphere(pos, ray, top_r).y;
            let ground = ray_sphere(pos, ray, planet_r);
            let hit_ground = ground.x > 0.0;
            if (hit_ground) {
                t_max = ground.x;
            }

            let cos_t = dot(ray, sun);
            let rp = rayleigh_phase(cos_t);
            let mp = mie_phase(cos_t);

            var lum = vec3<f32>(0.0);
            var lum_factor = vec3<f32>(0.0);
            var trans = vec3<f32>(1.0);
            var t = 0.0;
            for (var s = 0.0; s < MS_STEPS; s += 1.0) {
                let new_t = ((s + 0.5) / MS_STEPS) * t_max;
                let dt = new_t - t;
                t = new_t;
                let p = pos + ray * t;
                let m = scattering_values(p);
                let safe_ext = max(m.extinction, vec3<f32>(1e-9));
                let sample_trans = exp(-dt * m.extinction);

                // Isotropic transfer (no phase) — feeds the multiscatter feedback.
                let scat_no_phase = m.rayleigh + vec3<f32>(m.mie);
                let scat_f = (scat_no_phase - scat_no_phase * sample_trans) / safe_ext;
                lum_factor += trans * scat_f;

                // Second-order single scattering toward the sun (real phase).
                let sun_t = sample_transmittance(p, sun);
                let in_scat = (m.rayleigh * rp + vec3<f32>(m.mie) * mp) * sun_t;
                let scat_int = (in_scat - in_scat * sample_trans) / safe_ext;
                lum += scat_int * trans;
                trans = trans * sample_trans;
            }

            if (hit_ground) {
                let hit_p = pos + ray * t_max;
                let up = normalize(hit_p);
                let ndl = max(dot(up, sun), 0.0);
                lum += trans * sky.ground_albedo.xyz * ndl * sample_transmittance(hit_p, sun) / PI;
            }

            lum_total += lum * inv_samples;
            fms_total += lum_factor * inv_samples;
        }
    }

    // Closed-form infinite-scattering sum: L2 / (1 - f).
    let psi = lum_total / max(vec3<f32>(1.0) - fms_total, vec3<f32>(1e-3));
    return vec4<f32>(psi, 1.0);
}

// ---- Main sky pass -----------------------------------------------------------

fn raymarch_sky(pos: vec3<f32>, ray: vec3<f32>, sun: vec3<f32>, t_max: f32) -> vec3<f32> {
    let cos_t = dot(ray, sun);
    let rp = rayleigh_phase(cos_t);
    let mp = mie_phase(cos_t);
    let steps = max(sky.control.y, 4.0);

    var lum = vec3<f32>(0.0);
    var trans = vec3<f32>(1.0);
    var t = 0.0;
    for (var i = 0.0; i < steps; i += 1.0) {
        let new_t = ((i + 0.3) / steps) * t_max;
        let dt = new_t - t;
        t = new_t;
        let p = pos + ray * t;
        let m = scattering_values(p);
        let safe_ext = max(m.extinction, vec3<f32>(1e-9));
        let sample_trans = exp(-dt * m.extinction);

        let sun_t = sample_transmittance(p, sun);
        let psi = sample_multiscatter(p, sun);
        // Single scattering (phase * sun transmittance) + multiscatter (phase-less).
        let rayleigh_in = m.rayleigh * (rp * sun_t + psi);
        let mie_in = vec3<f32>(m.mie) * (mp * sun_t + psi);
        let in_scat = rayleigh_in + mie_in;
        let scat_int = (in_scat - in_scat * sample_trans) / safe_ext;
        lum += scat_int * trans;
        trans = trans * sample_trans;
    }
    return lum;
}

fn hable_partial(x: vec3<f32>) -> vec3<f32> {
    let a = 0.15; let b = 0.50; let c = 0.10; let d = 0.20; let e = 0.02; let f = 0.30;
    return ((x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * f)) - e / f;
}

fn hable(color: vec3<f32>) -> vec3<f32> {
    let w = vec3<f32>(11.2);
    return hable_partial(color) / hable_partial(w);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

// Interleaved gradient noise — ~1 LSB dither for the LDR-direct path's 8-bit write.
fn ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

// Linear sky radiance along a world direction: the atmosphere march + night-faded horizon
// haze + the authored night-sky floor, WITHOUT the sun disc (callers that want the disc add it
// against the true, unfloored `dir`). Shared by the fullscreen sky (fs_sky) and the reflection
// environment map (fs_sky_env), so the water surface reflects the exact same sky.
fn sky_radiance(dir: vec3<f32>) -> vec3<f32> {
    let sun = normalize(sky.sun_dir.xyz);
    let planet_r = sky.params.z;
    let top_r = planet_r + sky.params.w;
    let cam_alt = max(sky.night_zenith.w, 0.0);
    let pos = vec3<f32>(0.0, planet_r + cam_alt, 0.0);

    // Sun radiance scale x the authored exposure (radiance -> scene-referred).
    let radiance = sky.sun_dir.w * sky.params.y;

    // March a horizon-floored direction so the sky BELOW the horizon smoothly continues the
    // horizon colour instead of darkening into an ugly band (the terrain / water draws over it).
    let march_dir = normalize(vec3<f32>(dir.x, max(dir.y, 0.0), dir.z));
    let atmo = ray_sphere(pos, march_dir, top_r);
    var color = vec3<f32>(0.0);
    if (atmo.y > 0.0) {
        color = raymarch_sky(pos, march_dir, sun, atmo.y) * radiance;
    }

    // Night-faded horizon haze: blend toward the scene fog colour near the horizon so the fogged
    // distant terrain and the sky meet without a seam. Fades out at night (dark sky).
    let haze_strength = sky.fog_color.w * (1.0 - sky.ground_albedo.w);
    if (haze_strength > 0.0) {
        var fog = sky.fog_color.rgb;
        if (sky.output.x > 0.5) {
            fog = pow(max(fog, vec3<f32>(0.0)), vec3<f32>(2.2));
        }
        let th = (1.0 - smoothstep(0.0, 0.15, dir.y)) * haze_strength;
        color = mix(color, fog, clamp(th, 0.0, 1.0));
    }

    // Night-sky floor: an authored deep-blue that fills in as the sun drops below the horizon
    // (blended by sun altitude), so twilight/night settle into a believable blue instead of the
    // physical model's near-black. See docs/procedural-sky-plan.md Stage 6.
    let night_blend = 1.0 - smoothstep(sky.night_params.y, sky.night_params.x, sun.y);
    if (night_blend > 0.0) {
        let night = mix(sky.night_horizon.rgb, sky.night_zenith.rgb, clamp(dir.y, 0.0, 1.0));
        color = color + night * sky.night_params.z * night_blend;
    }

    return max(color, vec3<f32>(0.0));
}

@fragment
fn fs_sky(in: VsOut) -> @location(0) vec4<f32> {
    let dir = normalize(in.ray_dir);
    let sun = normalize(sky.sun_dir.xyz);
    let planet_r = sky.params.z;
    let cam_alt = max(sky.night_zenith.w, 0.0);
    let pos = vec3<f32>(0.0, planet_r + cam_alt, 0.0);

    var color = sky_radiance(dir);

    // Procedural sun disc, attenuated by atmospheric transmittance (reddens at low sun). Only when
    // the view ray misses the planet. Added here (not in sky_radiance) so the water reflection —
    // which uses its own analytic glint — doesn't double up on the sun.
    let ground = ray_sphere(pos, dir, planet_r);
    if (ground.x <= 0.0) {
        let cos_sun = dot(dir, sun);
        let cos_radius = cos(sky.params.x);
        if (cos_sun > cos_radius) {
            let sun_t = sample_transmittance(pos, sun);
            let edge = clamp((cos_sun - cos_radius) / (1.0 - cos_radius), 0.0, 1.0);
            let limb = 0.4 + 0.6 * sqrt(edge);
            let radiance = sky.sun_dir.w * sky.params.y;
            color = color + radiance * sun_t * limb;
        }
    }

    // Linear HDR path: hand linear radiance to the tonemap resolve. LDR-direct: no resolve runs,
    // so self-tonemap (exposure already baked into the radiance scale).
    if (sky.output.x < 0.5) {
        color = hable(color);
        color = linear_to_srgb(color);
        color += (ign(in.clip.xy) - 0.5) / 255.0;
    }
    return vec4<f32>(color, 1.0);
}

// Reflection environment map: bakes sky_radiance into an equirectangular (lat-long) texture the
// water surface samples in its reflected view direction. Always LINEAR radiance (disc-free), so
// water Fresnel-mixes it directly on the HDR path; the fullscreen sky/tonemap are unaffected.
// UV convention (must match water.wgsl's dir_to_equirect): u = azimuth, v = 0 at zenith .. 1 nadir.
@fragment
fn fs_sky_env(in: VsOut) -> @location(0) vec4<f32> {
    let azimuth = (in.uv.x - 0.5) * (2.0 * PI);
    let polar = in.uv.y * PI;              // 0 = up, PI = down
    let sp = sin(polar);
    let dir = vec3<f32>(sp * cos(azimuth), cos(polar), sp * sin(azimuth));
    return vec4<f32>(sky_radiance(dir), 1.0);
}

// ---- Aerial-perspective froxel volume (fill) ---------------------------------
// The Forward+-native replacement for the deferred fs_aerial pass: a frustum-aligned
// 3D volume where each froxel stores the atmosphere in-scattered toward the camera and
// the transmittance from the camera up to that froxel's distance. The forward shaders
// then apply fog with ONE trilinear tap at the fragment's froxel coordinate, so every
// fragment (terrain, object, foliage, transparent) fogs by its own distance and 2D is
// simply never sampled — no deferred depth readback, no render-order split.
//
// XY = screen; the Z slice maps to distance with a SQUARED distribution (dense near the
// camera): texel-centre w in [0,1] -> dist = max_dist * w^2, so sampling is
// w = sqrt(dist / max_dist). Filled once per frame by marching each column front-to-back
// and storing the running (inscatter, transmittance) at each slice — O(depth) per column.
// Reuses the exact sky atmosphere (LUTs + phase), so the froxel fog matches the sky.
// (Stage 1: fill only. Forward-shader sampling + sun-shadowing land in later stages.)
@group(0) @binding(5) var froxel_out: texture_storage_3d<rgba16float, write>;

// Group(1): the long-range terrain sun-shadow mask (same world-space "shadow ceiling"
// map the forward shaders sample via frame::terrain_sun_shadow), lent to the froxel fill
// so the fog is occluded by terrain — the sun stops shining THROUGH hills into the haze,
// and gaps between ridges become god-ray shafts. Standalone-validated shader, so the
// struct + sampling are duplicated here rather than imported. Matches TerrainShadowMap.
struct FroxelShadow {
    origin: vec2<f32>,     // world xz of the mask's (0,0)
    inv_span: vec2<f32>,   // world-xz -> [0,1] over the map
    half_texel: vec2<f32>, // 0.5 / mask_dims
    enabled: f32,          // 0 until a heightmap is loaded
    pad: f32,
};
@group(1) @binding(0) var shadow_mask: texture_2d<f32>;
@group(1) @binding(1) var shadow_samp: sampler;
@group(1) @binding(2) var<uniform> shadow_map: FroxelShadow;

// Occlusion [0,1] of the sun by terrain at an ABSOLUTE world position: 0 = lit, 1 = fully
// terrain-shadowed. Mirror of frame::terrain_sun_shadow (a point at (xz, y) is occluded by
// how far y sits below the column's shadow ceiling, softened by the penumbra half-width).
fn terrain_occlusion(world_xz: vec2<f32>, world_y: f32) -> f32 {
    if (shadow_map.enabled < 0.5) {
        return 0.0;
    }
    let uv = (world_xz - shadow_map.origin) * shadow_map.inv_span + shadow_map.half_texel;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return 0.0;
    }
    let sm = textureSampleLevel(shadow_mask, shadow_samp, uv, 0.0);
    let lit = smoothstep(sm.r - sm.g, sm.r + sm.g + 1e-3, world_y);
    return clamp(sm.b * (1.0 - lit), 0.0, 1.0);
}

// Cascade shadow map (near field, objects + terrain), so the fog is occluded by SHARP
// casters — tree trunks, buildings, ridgelines — which the smooth terrain-ceiling mask
// can't resolve. That's what carves crisp god-ray shafts. Mirrors WgrCameraShadow; the
// fog has no surface normal / screen derivatives, so this is a simplified single-tap
// version of frame::shadow_strength (no PCF, no normal/plane bias — just a constant depth
// bias), which is plenty for the soft 32^3 volume.
struct FroxelCsm {
    cascade_vp: array<mat4x4<f32>, 4>,
    splits: vec4<f32>,      // frustum tiers: far eye-depth per tier
    omni_radius: vec4<f32>, // omni tiers: camera-distance radius
    ctl: vec4<f32>,         // {count, omni_count, fade_range, bias_const}
    ctlb: vec4<f32>,        // {texel_size, darkness, normal_offset_scale, pcf}
    cam_fwd: vec4<f32>,     // xyz = camera forward (eye-depth cascade select)
    sun_dir: vec4<f32>,
};
@group(1) @binding(3) var csm_tex: texture_depth_2d_array;
@group(1) @binding(4) var csm_cmp: sampler_comparison;
@group(1) @binding(5) var<uniform> csm: FroxelCsm;

// Occlusion [0,1] of the sun by the cascade shadow map at a camera-relative position
// (1 = shadowed). Same cascade select + far-fade as shadow_strength, single compare tap.
fn csm_occlusion(pos: vec3<f32>) -> f32 {
    let n = i32(csm.ctl.x);
    if (n <= 0) {
        return 0.0;
    }
    let omni_n = i32(csm.ctl.y);
    let eye_depth = dot(pos, csm.cam_fwd.xyz);
    let dist3d = length(pos);
    var ci = n;
    for (var i = 0; i < 4; i++) {
        if (i >= n) {
            break;
        }
        let metric = select(eye_depth, dist3d, i < omni_n);
        if (metric <= csm.splits[i]) {
            ci = i;
            break;
        }
    }
    if (ci >= n) {
        return 0.0;
    }
    let cp = csm.cascade_vp[ci] * vec4<f32>(pos, 1.0);
    let sc = cp.xyz / cp.w;
    let suv = vec2<f32>(sc.x * 0.5 + 0.5, 0.5 - sc.y * 0.5);
    if (suv.x <= 0.0 || suv.x >= 1.0 || suv.y <= 0.0 || suv.y >= 1.0 || sc.z <= 0.0 || sc.z >= 1.0) {
        return 0.0;
    }
    let bias = csm.ctl.w * f32(ci + 1) * f32(ci + 1);
    let lit = textureSampleCompareLevel(csm_tex, csm_cmp, suv, ci, sc.z - bias);
    // Fade out over the last cascade's tail exactly like shadow_strength, so the near-field
    // CSM occlusion hands off to the long-range terrain mask without a hard edge.
    let fade = clamp((csm.splits[n - 1] - eye_depth) / max(csm.ctl.z, 0.001), 0.0, 1.0);
    return (1.0 - lit) * fade;
}

@compute @workgroup_size(8, 8, 1)
fn cs_froxel(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(froxel_out);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    // View ray for this column: unproject the pixel centre at the NEAR plane (NDC z = 0,
    // safe under the infinite-far projection), translation-free so the length is metric.
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    let world = sky.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let ray = normalize(world.xyz / world.w);

    let sun = normalize(sky.sun_dir.xyz);
    let cam_alt = max(sky.night_zenith.w, 0.0);
    let origin = vec3<f32>(0.0, sky.params.z + cam_alt, 0.0);
    let radiance = sky.sun_dir.w * sky.params.y;
    let max_dist = max(sky.night_params.w, 1.0);
    let depth = f32(dims.z);

    let cos_t = dot(ray, sun);
    let rp = rayleigh_phase(cos_t);
    let mp = mie_phase(cos_t);

    // Full sky radiance along this column (horizon-floored exactly like fs_sky, so a
    // downward terrain ray still resolves the HORIZON sky it should dissolve into). The
    // far froxels blend toward this so the terrain edge and newly-streamed tiles fade into
    // the real sky instead of the dim airlight-to-fog-range (which read as a grey band and
    // let geometry pop against the sky). This is the froxel-native far-fade.
    let march_dir = normalize(vec3<f32>(ray.x, max(ray.y, 0.0), ray.z));
    let atmo = ray_sphere(origin, march_dir, sky.params.z + sky.params.w);
    var sky_full = vec3<f32>(0.0);
    if (atmo.y > 0.0) {
        sky_full = raymarch_sky(origin, march_dir, sun, atmo.y) * radiance;
    }

    // March front-to-back, accumulating into each slice. Two analytic sub-steps per slice
    // keep the thick far froxels honest; the near ones are thin so it's cheap.
    let sub = 2u;
    var lum = vec3<f32>(0.0);
    var trans = vec3<f32>(1.0);
    var t_prev = 0.0;
    for (var z = 0u; z < dims.z; z = z + 1u) {
        let w_center = (f32(z) + 0.5) / depth;
        let t_target = max_dist * w_center * w_center;
        let seg = (t_target - t_prev) / f32(sub);
        for (var s = 0u; s < sub; s = s + 1u) {
            let t0 = t_prev + seg * f32(s);
            let dt = seg;
            let march = t0 + dt * 0.5;
            let p = origin + ray * march;
            let m = scattering_values(p);
            let safe_ext = max(m.extinction, vec3<f32>(1e-9));
            let sample_trans = exp(-dt * m.extinction);
            let sun_t = sample_transmittance(p, sun);
            let psi = sample_multiscatter(p, sun);
            // Occlude the DIRECT sun single-scatter by terrain (the froxel sample's absolute
            // world position = camera + the marched camera-relative offset). Multiscatter
            // (psi) stays as ambient fill, so shadowed fog is dim-but-not-black. This is what
            // stops the low sun bleeding through ridges and carves god-ray shafts.
            let world_off = ray * march;
            // Fog occlusion by the long-range terrain shadow-ceiling mask (absolute world pos).
            // CSM froxel occlusion is DISABLED: the cascade range is far shorter than where the
            // fog is dense, so it never overlaps foggy regions and had zero visible effect. The
            // path is kept fully wired (csm_occlusion + group(1) bindings 3-5, csm_ubo upload) so
            // re-enabling is a one-line change once the CSM range is extended:
            //     let occ = max(csm_occlusion(world_off), occ);
            // See the wgpu-renderer-project memory, "Stage 3b".
            let occ = terrain_occlusion(sky.cam_pos.xz + world_off.xz, sky.cam_pos.y + world_off.y);
            // night_horizon.w = user occlusion strength (0 = off for A/B; 1 = physical; >1 exaggerated).
            let sun_vis = clamp(1.0 - occ * sky.night_horizon.w, 0.0, 1.0);
            let in_scat = m.rayleigh * (rp * sun_t * sun_vis + psi) + vec3<f32>(m.mie) * (mp * sun_t * sun_vis + psi);
            let scat_int = (in_scat - in_scat * sample_trans) / safe_ext;
            lum += scat_int * trans;
            trans = trans * sample_trans;
        }
        t_prev = t_target;
        // Blend the physical airlight toward the full sky over the far half of the volume,
        // so w -> 1 (the draw edge) is the sky and geometry there dissolves seamlessly.
        let farfade = smoothstep(0.5, 1.0, w_center);
        let col = mix(lum * radiance, sky_full, farfade);
        let t_mono = dot(trans, vec3<f32>(0.2126, 0.7152, 0.0722)) * (1.0 - farfade);
        textureStore(froxel_out, vec3<i32>(i32(gid.x), i32(gid.y), i32(z)), vec4<f32>(col, t_mono));
    }
}
