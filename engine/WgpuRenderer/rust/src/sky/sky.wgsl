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
};

@group(0) @binding(0) var<uniform> sky: Sky;
@group(0) @binding(1) var lut_sampler: sampler;
@group(0) @binding(2) var transmittance_lut: texture_2d<f32>;
@group(0) @binding(3) var multiscatter_lut: texture_2d<f32>;
// Only bound (and only used) by the aerial-perspective pass (fs_aerial); the other
// entry points don't reference it, so their pipeline layouts omit binding 4.
@group(0) @binding(4) var depth_tex: texture_depth_2d;

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

@fragment
fn fs_sky(in: VsOut) -> @location(0) vec4<f32> {
    let dir = normalize(in.ray_dir);
    let sun = normalize(sky.sun_dir.xyz);
    let planet_r = sky.params.z;
    let top_r = planet_r + sky.params.w;
    let cam_alt = max(sky.night_zenith.w, 0.0);
    let pos = vec3<f32>(0.0, planet_r + cam_alt, 0.0);

    let ground = ray_sphere(pos, dir, planet_r);
    let hit_ground = ground.x > 0.0;

    // Sun radiance scale x the authored exposure (radiance -> scene-referred).
    let radiance = sky.sun_dir.w * sky.params.y;

    // March a horizon-floored direction so the sky BELOW the horizon smoothly
    // continues the horizon colour instead of darkening into an ugly band; the
    // terrain draws over it. Sun disc / ground tests still use the true `dir`.
    let march_dir = normalize(vec3<f32>(dir.x, max(dir.y, 0.0), dir.z));
    let atmo = ray_sphere(pos, march_dir, top_r);
    var color = vec3<f32>(0.0);
    if (atmo.y > 0.0) {
        color = raymarch_sky(pos, march_dir, sun, atmo.y) * radiance;
    }

    // Night-faded horizon haze: blend toward the scene fog colour near the horizon so
    // the fogged distant terrain and the sky meet without a seam (interim aerial
    // perspective, plan Stage 4). Fades out at night so it stops lighting the dark sky.
    let haze_strength = sky.fog_color.w * (1.0 - sky.ground_albedo.w);
    if (haze_strength > 0.0) {
        var fog = sky.fog_color.rgb;
        if (sky.output.x > 0.5) {
            fog = pow(max(fog, vec3<f32>(0.0)), vec3<f32>(2.2));
        }
        let th = (1.0 - smoothstep(0.0, 0.15, dir.y)) * haze_strength;
        color = mix(color, fog, clamp(th, 0.0, 1.0));
    }

    // Procedural sun disc, attenuated by atmospheric transmittance (reddens at low sun).
    // Only when the view ray misses the planet.
    if (!hit_ground) {
        let cos_sun = dot(dir, sun);
        let cos_radius = cos(sky.params.x);
        if (cos_sun > cos_radius) {
            let sun_t = sample_transmittance(pos, sun);
            let edge = clamp((cos_sun - cos_radius) / (1.0 - cos_radius), 0.0, 1.0);
            let limb = 0.4 + 0.6 * sqrt(edge);
            color = color + radiance * sun_t * limb;
        }
    }

    // Night-sky floor: an authored deep-blue that fills in as the sun drops below the
    // horizon (blended by sun altitude), so twilight/night settle into a believable
    // blue instead of the physical model's near-black. Added, so the sunset glow and
    // the emerging night coexist. See docs/procedural-sky-plan.md Stage 6.
    let night_blend = 1.0 - smoothstep(sky.night_params.y, sky.night_params.x, sun.y);
    if (night_blend > 0.0) {
        let night = mix(sky.night_horizon.rgb, sky.night_zenith.rgb, clamp(dir.y, 0.0, 1.0));
        color = color + night * sky.night_params.z * night_blend;
    }

    color = max(color, vec3<f32>(0.0));

    // Linear HDR path: hand linear radiance to the tonemap resolve. LDR-direct: no
    // resolve runs, so self-tonemap (exposure already baked into the radiance scale).
    if (sky.output.x < 0.5) {
        color = hable(color);
        color = linear_to_srgb(color);
        color += (ign(in.clip.xy) - 0.5) / 255.0;
    }
    return vec4<f32>(color, 1.0);
}

// ---- Aerial perspective (plan Stage 4) ---------------------------------------
// A deferred fullscreen pass over the finished scene depth. For each shaded pixel it
// marches the SAME atmosphere as the sky, but only from the camera to the pixel's
// world distance, producing the in-scattered haze in front of the surface and the
// transmittance of the surface radiance through it. Because it reuses fs_sky's exact
// functions and LUTs, distant terrain fades into precisely the sky colour above it —
// no separate fog colour, no horizon seam. Composited by hardware blend:
//   result = inscatter + surface * transmittance   (src=One, dst=SrcAlpha).

struct Aerial {
    inscatter: vec3<f32>,
    transmittance: vec3<f32>,
};

fn raymarch_aerial(pos: vec3<f32>, ray: vec3<f32>, sun: vec3<f32>, t_max: f32) -> Aerial {
    let cos_t = dot(ray, sun);
    let rp = rayleigh_phase(cos_t);
    let mp = mie_phase(cos_t);
    // Aerial perspective is low-frequency; a fixed modest step count is plenty and
    // the per-segment analytic integral keeps it energy-correct regardless.
    let steps = 12.0;

    var lum = vec3<f32>(0.0);
    var trans = vec3<f32>(1.0);
    var t = 0.0;
    for (var i = 0.0; i < steps; i += 1.0) {
        let new_t = ((i + 0.5) / steps) * t_max;
        let dt = new_t - t;
        t = new_t;
        let p = pos + ray * t;
        let m = scattering_values(p);
        let safe_ext = max(m.extinction, vec3<f32>(1e-9));
        let sample_trans = exp(-dt * m.extinction);

        let sun_t = sample_transmittance(p, sun);
        let psi = sample_multiscatter(p, sun);
        let rayleigh_in = m.rayleigh * (rp * sun_t + psi);
        let mie_in = vec3<f32>(m.mie) * (mp * sun_t + psi);
        let in_scat = rayleigh_in + mie_in;
        let scat_int = (in_scat - in_scat * sample_trans) / safe_ext;
        lum += scat_int * trans;
        trans = trans * sample_trans;
    }

    var r: Aerial;
    r.inscatter = lum;
    r.transmittance = trans;
    return r;
}

@fragment
fn fs_aerial(in: VsOut) -> @location(0) vec4<f32> {
    let d_buf = textureLoad(depth_tex, vec2<i32>(in.clip.xy), 0);
    // Reversed-Z: background (no geometry) keeps the cleared far value 0, where the
    // sky already drew the full atmosphere — leave it untouched (blend identity).
    if (d_buf <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Un-reverse to forward NDC z, then unproject with the translation-free
    // inv_view_proj. Rotation preserves length, so the result's magnitude is the
    // camera->fragment distance and its direction is the world view ray — no separate
    // inverse-projection needed.
    let ndc = in.uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    let world_h = sky.inv_view_proj * vec4<f32>(ndc, 1.0 - d_buf, 1.0);
    let offset = world_h.xyz / world_h.w;
    let frag_dist = length(offset);
    let ray = offset / frag_dist;

    let sun = normalize(sky.sun_dir.xyz);
    let cam_alt = max(sky.night_zenith.w, 0.0);
    let pos = vec3<f32>(0.0, sky.params.z + cam_alt, 0.0);
    let radiance = sky.sun_dir.w * sky.params.y;

    let ap = raymarch_aerial(pos, ray, sun, frag_dist);
    var inscatter = ap.inscatter * radiance;
    var trans = ap.transmittance;

    // Far-fade: with the infinite-far projection the terrain edge sits at a finite
    // distance (the fog/view range) while the sky above it is fogged to infinity, so
    // the two meet in a soft colour step. As the fragment nears that range, dissolve
    // the surface into the FULL sky radiance along its ray — at fade = 1 the terrain
    // pixel equals the sky directly above it, so the horizon is seamless. This is also
    // what rounds off the peripheral over-draw of the legacy flat-far-plane cull.
    // night_params.w carries the range (0 = disabled, e.g. no scene).
    let fog_far = sky.night_params.w;
    if (fog_far > 0.0) {
        let fade = smoothstep(fog_far * 0.9, fog_far, frag_dist);
        if (fade > 0.0) {
            // Horizon-floored ray so it matches fs_sky's below-horizon continuation.
            let march_dir = normalize(vec3<f32>(ray.x, max(ray.y, 0.0), ray.z));
            let atmo = ray_sphere(pos, march_dir, sky.params.z + sky.params.w).y;
            var full_sky = vec3<f32>(0.0);
            if (atmo > 0.0) {
                full_sky = raymarch_sky(pos, march_dir, sun, atmo) * radiance;
            }
            // Match fs_sky's night floor so the fade is seamless after dusk too.
            let night_blend = 1.0 - smoothstep(sky.night_params.y, sky.night_params.x, sun.y);
            if (night_blend > 0.0) {
                let night = mix(sky.night_horizon.rgb, sky.night_zenith.rgb, clamp(ray.y, 0.0, 1.0));
                full_sky += night * sky.night_params.z * night_blend;
            }
            inscatter = mix(inscatter, full_sky, fade);
            trans = trans * (1.0 - fade);
        }
    }

    // Blend uses a single alpha, so carry luminance-weighted (grey) transmittance.
    // Chromatic extinction of the background is a later refinement (needs ping-pong).
    let t_mono = dot(trans, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>(inscatter, t_mono);
}
