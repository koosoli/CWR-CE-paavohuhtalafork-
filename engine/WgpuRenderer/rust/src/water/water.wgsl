// GPU water: a CDLOD surface at the global sea level, displaced by a sum of Gerstner
// waves in the vertex shader and shaded per-fragment with an analytic wave normal, a
// sharp HDR sun specular, and a Fresnel mix toward the horizon tint. The shared grid
// mesh is instanced per node (mirroring terrain) and camera-relative, reversed-Z.
// Shares group(0) (camera UBO + aerial fog) so distant water dissolves into the sky.
//
// Waves are purely cosmetic vertex displacement — gameplay reads the flat sea plane.
// Depth-based colour, refraction and planar reflection are later stages of the water
// look plan; this stage adds shape + glint + reduced transparency, no new render targets.
// The look params are UBO fields (not pipeline overrides) so the Water ImGui tab tunes
// them live.

#import frame::{frame, reverse_z, fog_factor, apply_fog, terrain_sun_shadow}
#import shadow::shadow_strength
#import color::srgb_to_linear

struct WaterParams {
    world_origin: vec2<f32>,
    terrain_grid: f32,
    sea_level: f32,
    hm_width: u32,
    hm_height: u32,
    time: f32,
    wave_amp: f32,       // overall amplitude scale
    wave_choppy: f32,    // horizontal steepness
    wave_speed: f32,     // animation speed
    wave_scale: f32,     // wavelength scale (>1 = larger, farther-apart waves)
    fade_start: f32,     // distance (m) where wave detail starts flattening
    fade_end: f32,       // distance (m) where water is fully flat (kills far moiré/repetition)
    warp_amp: f32,       // de-tiling domain-warp amplitude (m)
    spec_power: f32,     // sun-specular sharpness
    spec_intensity: f32, // sun-specular brightness (HDR, blooms)
    alpha: f32,          // base opacity (Fresnel raises it toward 1 at grazing angles)
    shadow_dim: f32,     // extra darkening of shadowed water (0 = sun-only removal)
};

// Must match GRID_N in water/mod.rs (and the terrain grid).
const GRID_N: f32 = 32.0;

@group(1) @binding(0) var<uniform> wp: WaterParams;

// Hard-coded wave set (per-map authoring can come later). Each: dir.xy (un-normalised),
// wavelength (m), amplitude (m). Gentle by design — the amplitudes sum to ~0.25 m, the
// legacy maxWave, so crests never lift a hull off the flat buoyancy plane.
const NUM_WAVES: i32 = 4;
const WAVES = array<vec4<f32>, 4>(
    vec4<f32>( 1.0,  0.15, 27.0, 0.110),
    vec4<f32>( 0.6,  0.80, 15.0, 0.070),
    vec4<f32>(-0.5,  0.90,  9.0, 0.045),
    vec4<f32>( 0.9, -0.40,  5.5, 0.030),
);
const G: f32 = 9.81;      // gravity, for the deep-water dispersion omega = sqrt(g*k)
const TWO_PI: f32 = 6.2831853;

// Per-wave steepness, normalised by 1/(k*A*N) so the summed horizontal displacement
// stays below the loop-forming limit regardless of the chosen choppiness.
fn wave_steepness(k: f32, a: f32) -> f32 {
    return wp.wave_choppy / max(k * a * f32(NUM_WAVES), 1e-4);
}

// 1 near the camera, ramping to 0 by fade_end so ALL wave detail (short and long)
// flattens with distance — this is what removes the airplane-view moiré and the far
// repetition: past fade_end the water is a smooth mirror of the horizon tint, which is
// also how distant water genuinely reads.
fn wave_fade(dist: f32) -> f32 {
    return 1.0 - smoothstep(wp.fade_start, wp.fade_end, dist);
}

// Slowly-varying position offset (two incommensurate octaves) that bends the wave field
// off the regular grid. Low-frequency so its gradient is small and the analytic normal
// (evaluated at the warped position) stays accurate to first order.
fn domain_warp(p: vec2<f32>) -> vec2<f32> {
    let w1 = vec2<f32>(sin(p.y * 0.024 + 1.7), sin(p.x * 0.024 + 4.2));
    let w2 = vec2<f32>(sin(p.y * 0.057 + 3.1), sin(p.x * 0.057 + 0.6));
    return wp.warp_amp * (w1 + 0.5 * w2);
}

// Sum of Gerstner horizontal + vertical displacement at base world-xz `p_in`, time
// `wp.time`, faded by distance so far water flattens (no geometric aliasing).
fn gerstner_disp(p_in: vec2<f32>, dist: f32) -> vec3<f32> {
    let fade = wave_fade(dist);
    let p = p_in + domain_warp(p_in);
    let scale = max(wp.wave_scale, 0.01);
    var disp = vec3<f32>(0.0);
    for (var i = 0; i < NUM_WAVES; i = i + 1) {
        let w = WAVES[i];
        let d = normalize(w.xy);
        let a = w.w * wp.wave_amp * fade;
        let k = TWO_PI / (w.z * scale);
        let omega = sqrt(G * k) * wp.wave_speed;
        let phase = k * dot(d, p) - omega * wp.time;
        let q = wave_steepness(k, a);
        let c = cos(phase);
        disp.x = disp.x + q * a * d.x * c;
        disp.z = disp.z + q * a * d.y * c;
        disp.y = disp.y + a * sin(phase);
    }
    return disp;
}

// Analytic surface normal of the same Gerstner sum, evaluated per-fragment (like
// terrain's per-fragment normal) so specular stays crisp independent of tessellation.
// The same distance fade flattens the far-field normal, removing the specular shimmer.
fn gerstner_normal(p_in: vec2<f32>, dist: f32) -> vec3<f32> {
    let fade = wave_fade(dist);
    let p = p_in + domain_warp(p_in);
    let scale = max(wp.wave_scale, 0.01);
    var nx = 0.0;
    var ny = 0.0;
    var nz = 0.0;
    for (var i = 0; i < NUM_WAVES; i = i + 1) {
        let w = WAVES[i];
        let d = normalize(w.xy);
        let a = w.w * wp.wave_amp * fade;
        let k = TWO_PI / (w.z * scale);
        let omega = sqrt(G * k) * wp.wave_speed;
        let phase = k * dot(d, p) - omega * wp.time;
        let q = wave_steepness(k, a);
        let wa = k * a;
        let c = cos(phase);
        let s = sin(phase);
        nx = nx - d.x * wa * c;
        nz = nz - d.y * wa * c;
        ny = ny - q * wa * s;
    }
    return normalize(vec3<f32>(nx, 1.0 + ny, nz));
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>, // camera-relative displaced position
    @location(1) base_xz: vec2<f32>,   // undisplaced world-xz (for per-fragment normal)
    @location(2) fog: f32,             // 1 = keep colour, 0 = full fog
};

override skirt_k: f32 = 0.0;

@vertex
fn vs_water(
    @location(0) grid_in: vec3<f32>, // xy = unit grid position in [0,1]^2, z = skirt flag
    @location(1) origin: vec2<f32>,  // node world-xz origin
    @location(2) size: f32,          // node world size
    @location(3) lod: u32,
    @location(4) morph: vec2<f32>,   // (morph_start, morph_end) camera-distance band
) -> VsOut {
    let grid = grid_in.xy;
    let world_xz_fine = origin + grid * size;
    let dist = length(vec3<f32>(world_xz_fine.x, wp.sea_level, world_xz_fine.y) - frame.cam_pos.xyz);

    // Morph toward the coarser even lattice near the LOD boundary; both the wave sample
    // and the drawn geometry use this morphed base so adjacent levels agree (crack-free).
    var morph_k = 0.0;
    if (morph.y > morph.x)
    {
        morph_k = clamp((dist - morph.x) / (morph.y - morph.x), 0.0, 1.0);
    }
    let gidx = grid * GRID_N;
    let grid_coarse = (round(gidx * 0.5) * 2.0) / GRID_N;
    let base_xz = origin + mix(grid, grid_coarse, morph_k) * size;

    let disp = gerstner_disp(base_xz, dist);
    let y = wp.sea_level + disp.y - grid_in.z * (size / GRID_N) * skirt_k;
    let world_rel = vec3<f32>(base_xz.x + disp.x, y, base_xz.y + disp.z) - frame.cam_pos.xyz;

    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * vec4<f32>(world_rel, 1.0));
    out.world_pos = world_rel;
    out.base_xz = base_xz;
    out.fog = fog_factor(length(world_rel));
    return out;
}

// HDR path: 1 = decode tint/fog to linear, keep the glint un-clamped so it blooms.
override linear: f32 = 0.0;

@fragment
fn fs_water(in: VsOut) -> @location(0) vec4<f32> {
    // Receiver-plane derivatives for the CSM bias must be taken in uniform control flow.
    let dwx = dpdx(in.world_pos);
    let dwy = dpdy(in.world_pos);

    let n = gerstner_normal(in.base_xz, length(in.world_pos));
    let v = normalize(-in.world_pos);              // surface -> camera
    let l = normalize(-frame.sun_dir_world.xyz);   // surface -> sun

    // Sun occlusion: CSM cascades (objects/near contact) and the long-range terrain
    // heightfield mask compose by max() — whichever occludes the sun more wins. Both
    // fade with distance fog. 1 = fully shadowed. This removes the sun glint + direct
    // sheen and (via shadow_dim) darkens the shadowed water — e.g. a headland's shadow.
    let world_y = in.world_pos.y + frame.cam_pos.y;
    let csm_s = shadow_strength(in.world_pos, n, in.fog, dwx, dwy);
    let ter_s = terrain_sun_shadow(in.base_xz, world_y) * in.fog;
    let sun_shadow = max(csm_s, ter_s);
    let sun_vis = 1.0 - sun_shadow;

    var sun_diffuse = frame.sun_diffuse.rgb;
    var sun_ambient = frame.sun_ambient.rgb;
    var fog_color = frame.fog_color.rgb;
    // sun_diffuse.w = 1: sky-based lighting is already physical linear radiance.
    let sky_lit = frame.sun_diffuse.w > 0.5;
    if (linear > 0.5) {
        if (!sky_lit) {
            sun_diffuse = srgb_to_linear(sun_diffuse);
            sun_ambient = srgb_to_linear(sun_ambient);
        }
        fog_color = srgb_to_linear(fog_color);
    }

    // Deep-water body colour (weakly diffuse — water mostly reflects/transmits).
    var body = vec3<f32>(0.015, 0.075, 0.105);
    if (linear > 0.5) {
        body = srgb_to_linear(body);
    }
    // Direct-sun diffuse sheen is removed in shadow; sky ambient survives.
    let ndl = max(dot(n, l), 0.0);
    var rgb = body * (sun_ambient + sun_diffuse * ndl * 0.15 * sun_vis);

    // Fresnel toward the horizon/sky tint (a cheap reflection stand-in until Stage 4's
    // real sky reflection): near-grazing water lightens and reads reflective.
    let ndv = max(dot(n, v), 0.0);
    let f0 = 0.02;
    // max() guards pow() against a tiny negative base from normalize() rounding (NaN).
    let fresnel = f0 + (1.0 - f0) * pow(max(1.0 - ndv, 0.0), 5.0);
    rgb = mix(rgb, fog_color, fresnel);

    // Sharp HDR sun glint (Blinn-Phong); un-clamped so the bloom pass catches it. The
    // sun is occluded in shadow, so the glint vanishes there.
    let h = normalize(l + v);
    let spec = pow(max(dot(n, h), 0.0), wp.spec_power) * wp.spec_intensity * sun_vis;
    rgb = rgb + sun_diffuse * spec;

    // Artistic darkening of shadowed water for readability (dims the ambient/sky term
    // too, which the pure sun-removal above does not). 0 = physical (sun-only).
    rgb = rgb * (1.0 - sun_shadow * wp.shadow_dim);

    // LDR path saturates like the rest of the LDR pipeline; HDR keeps radiance uncapped.
    if (linear <= 0.5) {
        rgb = min(rgb, vec3<f32>(1.0));
    }

    if (frame.params.fog_enabled >= 1.5) {
        rgb = apply_fog(rgb, in.world_pos);
    } else {
        rgb = mix(fog_color, rgb, in.fog);
    }

    // Base opacity, Fresnel-opaque at grazing angles (where real water is a mirror) so
    // the seabed only shows through looking down.
    let alpha = mix(wp.alpha, 1.0, fresnel);
    return vec4<f32>(rgb, alpha);
}
