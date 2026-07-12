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

#import frame::{frame, reverse_z, fog_factor, apply_fog, terrain_sun_shadow, sky_vis_ao}
#import shadow::shadow_strength
#import color::srgb_to_linear

const PI: f32 = 3.14159265359;

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
    color_ext: f32,      // 1/m: body tint saturates shallow->deep over ~1/ext metres of depth
    coast_fade: f32,     // m of column depth over which the shore ramps transparent->opaque
    shallow_color: vec4<f32>, // rgb = shallow body tint (gamma-space; decoded to linear on HDR)
    deep_color: vec4<f32>,    // rgb = deep body tint
    foam_width: f32,     // m of column depth over which shoreline foam fades out
    foam_intensity: f32, // foam brightness/coverage scale
    swash_amp: f32,      // m the near-shore waterline oscillates in/out (cosmetic)
    swash_speed: f32,    // swash cycles per second
};

// Must match GRID_N in water/mod.rs (and the terrain grid).
const GRID_N: f32 = 32.0;

@group(1) @binding(0) var<uniform> wp: WaterParams;
// Opaque scene depth from the prepass, farthest-sample resolved (single-sample: 1x aspect or the
// MSAA far-resolve). Used to reconstruct the seabed and hence the water column depth for depth-based
// colour + soft shore + foam. The FARTHEST sample matters: a nearest resolve would read A2C foliage
// / rotor edges as the "seabed", collapsing water_depth to ~0 and ringing them with foam.
@group(1) @binding(1) var scene_depth: texture_depth_2d;
// Sky reflection environment map (equirect, linear radiance) + its sampler (U wraps, V clamps),
// for the Stage-4a real sky reflection: sampled in the reflected view direction (HDR path only).
@group(1) @binding(2) var sky_env: texture_2d<f32>;
@group(1) @binding(3) var sky_env_samp: sampler;

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

// Water column depth (m) at this fragment: reconstruct the seabed's camera-relative position
// from the opaque prepass depth and take the vertical gap to the water surface. In the fragment
// shader `in.clip` is the framebuffer position (window pixels + depth), so its xy indexes the
// depth texel directly (framebuffer-resolution). The seabed shares this fragment's view ray, so
// its ndc.xy is the surface's ndc.xy (reproject world_pos); combine with the stored reversed-Z
// depth (forward ndc.z = 1 - stored) and inv_view_proj to unproject. Clamp >= 0.
const DEEP: f32 = 1000.0; // "no seabed behind" fallback column depth (metres)

fn seabed_depth(frag_xy: vec2<f32>, surface_rel: vec3<f32>) -> f32 {
    let d = textureLoad(scene_depth, vec2<i32>(frag_xy), 0);
    // d ~ 0 is the reversed-Z far/cleared value: no opaque seabed was drawn behind this pixel
    // (beyond the terrain/fog extent). Treat as maximally deep and skip the unproject, which
    // would divide by a ~0 w there.
    if (d <= 1e-6) {
        return DEEP;
    }
    let clip_s = frame.proj * frame.view * vec4<f32>(surface_rel, 1.0);
    let ndc_xy = clip_s.xy / clip_s.w;
    let seabed_h = frame.inv_view_proj * vec4<f32>(ndc_xy, 1.0 - d, 1.0);
    let seabed_rel = seabed_h.xyz / seabed_h.w;
    return max(surface_rel.y - seabed_rel.y, 0.0);
}

// Cheap value noise for the shoreline foam (churning band, no texture).
// Integer-cell bit hash → white-noise value in [0,1). A sin(dot())*large hash (the usual WGSL
// one-liner) loses all precision once world coords reach the thousands — OFP islands are ~12 km —
// and collapses into big axis-aligned blocks that read as a badly-tiled repeating texture. Hashing
// the integer cell index with bit ops is exact at any magnitude. `cell` is already integer-valued
// (vnoise floors before calling); the & 0xffff wrap only repeats every 65536 cells (far off-map).
fn hash2(cell: vec2<f32>) -> f32 {
    let c = vec2<u32>(vec2<i32>(cell) & vec2<i32>(0xffff));
    var n = c.x * 1597334677u + c.y * 3812015801u;
    n = (n ^ (n >> 15u)) * 2246822519u;
    n = n ^ (n >> 13u);
    return f32(n & 0xffffffu) / f32(0x1000000u);
}
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2(i);
    let b = hash2(i + vec2<f32>(1.0, 0.0));
    let c = hash2(i + vec2<f32>(0.0, 1.0));
    let d = hash2(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}
// Two octaves scrolling in different directions so the foam churns rather than sitting still.
const FOAM_FREQ: f32 = 0.35; // spatial frequency (per metre)
fn foam_noise(p_world: vec2<f32>, t: f32) -> f32 {
    var v = 0.6 * vnoise(p_world * FOAM_FREQ + vec2<f32>(t * 0.6, t * 0.2));
    v = v + 0.4 * vnoise(p_world * FOAM_FREQ * 2.1 - vec2<f32>(t * 0.3, t * 0.5));
    return v;
}

// Equirect lookup into the sky reflection env map. Matches fs_sky_env's convention in sky.wgsl:
// u = azimuth (atan2(z, x)/2pi + 0.5, U-wrapped), v = 0 at zenith .. 1 at nadir (acos(y)/pi).
// `dir` is a world-space direction. Returns linear sky radiance.
fn sky_env_sample(dir: vec3<f32>) -> vec3<f32> {
    let u = 0.5 + atan2(dir.z, dir.x) / TWO_PI;
    let v = acos(clamp(dir.y, -1.0, 1.0)) / (TWO_PI * 0.5);
    return textureSampleLevel(sky_env, sky_env_samp, vec2<f32>(u, v), 0.0).rgb;
}

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
    // Raw terrain sun-occlusion (1 = a ridge blocks the sun above this point) — reused below to
    // occlude the sky reflection where a hill stands between the water and the sun.
    let ter_raw = terrain_sun_shadow(in.base_xz, world_y);
    let ter_s = ter_raw * in.fog;
    let sun_shadow = max(csm_s, ter_s);
    // A sub-horizon sun casts no shadow, so shadowing alone can't remove its glint/sheen before
    // sunrise (the water was reading a bright specular reflection of a sun still under the horizon).
    // Gate the DIRECT sun on its elevation: l.y = surface->sun.y = sin(elevation), 0 at the horizon.
    // Ambient/sky (twilight tint) is untouched, so the pre-dawn colour still comes through.
    let sun_up = smoothstep(0.0, 0.06, l.y);
    let sun_vis = (1.0 - sun_shadow) * sun_up;

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

    // Depth-based body colour: turquoise shallows -> dark blue depths, saturating with the water
    // column depth like Beer-Lambert extinction. Reconstructed from the (farthest-resolved) opaque
    // prepass depth. Colours/extinction are live WgrWaterParams (Water tab).
    let water_depth = seabed_depth(in.clip.xy, in.world_pos);
    var shallow = wp.shallow_color.rgb;
    var deep = wp.deep_color.rgb;
    if (linear > 0.5) {
        shallow = srgb_to_linear(shallow);
        deep = srgb_to_linear(deep);
    }
    let depth_tint = 1.0 - exp(-water_depth * wp.color_ext);
    // Weakly diffuse — water mostly reflects/transmits — so this is the transmitted body tint.
    let body = mix(shallow, deep, depth_tint);
    // Direct-sun diffuse sheen is removed in shadow; sky ambient survives. Sky-visibility AO scales
    // only the diffuse sky ambient (and the foam ambient below) — NOT the env-map reflection term,
    // which is a directional specular reflection whose own occlusion is Stage 4b's job. Off (1.0) when
    // sky_vis_strength = 0. Subtle on grazing (reflection-dominated) water; mainly darkens shaded coves.
    let amb_ao = sky_vis_ao(in.base_xz);
    let ndl = max(dot(n, l), 0.0);
    var rgb = body * (sun_ambient * amb_ao + sun_diffuse * ndl * 0.15 * sun_vis);

    // Fresnel toward the horizon/sky tint (a cheap reflection stand-in until Stage 4's real sky
    // reflection): near-grazing water lightens and reads reflective.
    let ndv = max(dot(n, v), 0.0);
    let f0 = 0.02;
    // max() guards pow() against a tiny negative base from normalize() rounding (NaN).
    let fresnel = f0 + (1.0 - f0) * pow(max(1.0 - ndv, 0.0), 5.0);
    // Stage 4a: reflect the REAL sky. Sample the sky env map (disc-free atmosphere radiance) in the
    // reflected view direction — so night water reflects a genuinely dark sky, and only the fragments
    // that geometrically reflect toward the sun/horizon pick up its glow (no uniform pink wash). The
    // LDR-direct reference path has no linear env, so it keeps the sun-elevation-dimmed fog_color
    // stand-in. reflect(incident, n): incident = camera->surface world dir = normalize(world_pos).
    var refl: vec3<f32>;
    if (linear > 0.5) {
        let refl_dir = reflect(normalize(in.world_pos), n);
        var sky_refl = sky_env_sample(refl_dir);
        // The env map has no terrain, so a grazing reflection toward the low sun mirrors the bright
        // horizon glow even where a hill actually blocks that direction (a mountain between the water
        // and the sun). Approximate the missing occlusion with the terrain sun-shadow: where the sun
        // is ridge-occluded here AND the reflection points toward it, fade the reflected glow to the
        // (shadowed) sky ambient. A full fix is Stage 4b — reflecting the actual terrain.
        let toward_sun = smoothstep(0.0, 0.4, dot(refl_dir, l));
        sky_refl = mix(sky_refl, sun_ambient, ter_raw * toward_sun);
        refl = sky_refl;
    } else {
        refl = mix(sun_ambient, fog_color, sun_up);
    }
    rgb = mix(rgb, refl, fresnel);

    // Sharp HDR sun glint — the reflected sun disc, on the same physical scale as the sky's now
    // eye-searing sun. sun_diffuse is the solar IRRADIANCE (the scale that drives the sky); an
    // ENERGY-NORMALISED Blinn-Phong lobe spreads it over the highlight, so a tighter lobe (higher
    // spec_power) concentrates the same energy into a brighter, searing core instead of the old
    // flat irradiance-scale smear. Fresnel-weighted (microfacet v.h): a specular reflection of the
    // sun is Fresnel-modulated — water barely glints at normal incidence and hard along the grazing
    // glitter path. spec_intensity is now an artist trim (was the raw peak scale). Un-clamped so the
    // bloom pass catches it; the sun is occluded in shadow, so the glint vanishes there.
    let h = normalize(l + v);
    let ndh = max(dot(n, h), 0.0);
    let spec_fresnel = f0 + (1.0 - f0) * pow(max(1.0 - dot(v, h), 0.0), 5.0);
    let spec_norm = (wp.spec_power + 2.0) / (8.0 * PI);
    let spec = pow(ndh, wp.spec_power) * spec_norm * spec_fresnel * wp.spec_intensity * sun_vis;
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

    // Swash: a gentle oscillation of the near-shore waterline (cosmetic — buoyancy stays flat).
    // It raises/lowers the EFFECTIVE column depth so the transparent edge + foam breathe in and
    // out over the wet beach. The body colour keeps the true depth so it doesn't flicker.
    let swash = sin(TWO_PI * wp.time * wp.swash_speed);
    let eff_depth = water_depth + swash * wp.swash_amp;

    // Shoreline foam: churning procedural noise in a band along the (swash-moved) edge. The band
    // fades IN from the waterline and OUT into deeper water (peak ~1/4 of foam_width in), so the
    // LAND side dissolves softly instead of ending on a hard bright line where the water geometry
    // is clipped by the beach — the deep side already faded. Foam also fades to 0 right at the
    // edge, so the water there is transparent (soft wash over wet sand) rather than opaque white.
    let ft = eff_depth / max(wp.foam_width, 1e-4);
    let foam_band = smoothstep(0.0, 0.25, ft) * (1.0 - smoothstep(0.25, 1.0, ft));
    let foam = clamp(foam_band * foam_noise(in.base_xz, wp.time) * wp.foam_intensity, 0.0, 1.0);
    // Foam is bright diffuse spray, not an emitter: light it by the sky ambient + direct sun (where
    // the water isn't shadowed) so it goes dim at night instead of glowing white in the dark.
    let foam_color = sun_ambient * amb_ao + sun_diffuse * sun_vis;
    rgb = mix(rgb, foam_color, foam);

    // Base opacity, Fresnel-opaque at grazing angles (where real water is a mirror) so
    // the seabed only shows through looking down. A soft shoreline fade dissolves the water
    // to transparent as the (swash-moved) column depth -> 0, so the hard coast cut becomes a
    // gentle wash over the visible wet beach; foam forces opacity where it sits.
    let shore = smoothstep(0.0, wp.coast_fade, eff_depth);
    let alpha = max(mix(wp.alpha, 1.0, fresnel) * shore, foam);
    return vec4<f32>(rgb, alpha);
}
