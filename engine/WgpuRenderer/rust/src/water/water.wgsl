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
    fft_control: vec4<f32>, // enabled, seed, minimum geometry wavelength, pad
    fft_wind_sea: vec4<f32>, // wind x/z, speed, sea state
    fft_cascade_lengths: vec4<f32>, // stable world-space cascade lengths
    flow_direction_speed: vec4<f32>, // x/z direction, m/s, water kind (0 ocean, 1 river)
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
@group(1) @binding(4) var interaction_field: texture_2d<f32>;
@group(1) @binding(5) var interaction_samp: sampler;
@group(1) @binding(6) var fft_displacement: texture_2d_array<f32>;
@group(1) @binding(7) var fft_dynamics: texture_2d_array<f32>;
@group(1) @binding(8) var fft_auxiliary: texture_2d_array<f32>;
@group(1) @binding(9) var fft_samp: sampler;
@group(1) @binding(10) var foam_history: texture_2d<f32>;
@group(1) @binding(11) var foam_samp: sampler;
// Completed opaque HDR scene snapshot. It is resolved/copied before water starts, never
// sampled from the colour target currently being blended into.
@group(1) @binding(12) var scene_color: texture_2d<f32>;
@group(1) @binding(13) var scene_samp: sampler;
@group(1) @binding(14) var planar_color: texture_2d<f32>;
@group(1) @binding(15) var planar_samp: sampler;
struct PlanarParams { full_vp: mat4x4<f32>, valid: vec4<f32> };
@group(1) @binding(16) var<uniform> planar: PlanarParams;

// Multi-band open-ocean carrier. The long swells establish the readable sea state,
// while successively shorter cross-waves break up the regularity around the camera.
// The forthcoming Hydro FFT backend replaces this analytic spectrum but samples through
// the same CDLOD surface path, so the engine integration remains stable in the interim.
const NUM_WAVES: i32 = 8;
const WAVES = array<vec4<f32>, 8>(
    vec4<f32>( 0.92,  0.38, 70.0, 1.100),
    vec4<f32>( 0.42,  0.91, 39.0, 0.650),
    vec4<f32>(-0.61,  0.79, 24.0, 0.380),
    vec4<f32>( 0.97, -0.22, 15.0, 0.200),
    vec4<f32>(-0.20, -0.98,  9.0, 0.120),
    vec4<f32>( 0.70,  0.72,  5.0, 0.065),
    vec4<f32>(-0.94,  0.34,  3.0, 0.035),
    vec4<f32>( 0.20, -0.98,  1.8, 0.015),
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

// Sample absolute xz so camera-relative rendering never changes FFT phase.
fn fft_sample(xz: vec2<f32>, layer: i32) -> vec4<f32> {
    let length_m = max(wp.fft_cascade_lengths[layer], 1.0);
    return textureSampleLevel(fft_displacement, fft_samp, fract(xz / length_m), layer, 0.0);
}
fn fft_geometry_disp(xz: vec2<f32>, dist: f32) -> vec3<f32> {
    var disp = vec3<f32>(0.0);
    // The JONSWAP wind peak sits in the shortest 48 m cascade at the default
    // wind speed. It must contribute to geometry as well as normals or the
    // visible ocean stays flat while only its shading ripples.
    for (var layer = 0; layer < 4; layer = layer + 1) {
        disp = disp + fft_sample(xz, layer).xyz;
    }
    return disp * wave_fade(dist);
}
fn fft_normal(xz: vec2<f32>, dist: f32) -> vec3<f32> {
    var slope = vec2<f32>(0.0);
    for (var layer = 0; layer < 4; layer = layer + 1) {
        let length_m = max(wp.fft_cascade_lengths[layer], 1.0);
        slope = slope + textureSampleLevel(fft_dynamics, fft_samp, fract(xz / length_m), layer, 0.0).xy;
    }
    return normalize(vec3<f32>(-slope.x * wave_fade(dist), 1.0, -slope.y * wave_fade(dist)));
}

// The interaction field follows the camera in a 256m domain. Outside it samples zero;
// zero events therefore leave the established Gerstner-only path bit-for-bit unchanged.
fn interaction_sample(xz: vec2<f32>) -> vec4<f32> {
    let domain_origin = vec2<f32>(floor((frame.cam_pos.x - 128.0) / 4.0) * 4.0, floor((frame.cam_pos.z - 128.0) / 4.0) * 4.0);
    let uv = (xz - domain_origin) / 256.0;
    let inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    return textureSampleLevel(interaction_field, interaction_samp, clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0) * inside;
}
// Foam shares the interaction's snapped 256 m camera-relative domain, but its history is
// reprojected by the compute pass so this stable world lookup does not swim with the camera.
fn persistent_foam_sample(xz: vec2<f32>) -> vec4<f32> {
    let domain_origin = vec2<f32>(floor((frame.cam_pos.x - 128.0) / 4.0) * 4.0, floor((frame.cam_pos.z - 128.0) / 4.0) * 4.0);
    let uv = (xz - domain_origin) / 256.0;
    let inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    return textureSampleLevel(foam_history, foam_samp, clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0) * inside;
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

    var disp = gerstner_disp(base_xz, dist);
    if (wp.fft_control.x > 0.5) {
        disp = fft_geometry_disp(base_xz, dist);
    }
    let interaction = interaction_sample(base_xz);
    let y = wp.sea_level + disp.y + interaction.r - grid_in.z * (size / GRID_N) * skirt_k;
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

// Recover the world-xz gradient of the reconstructed column depth from screen derivatives.
// A cleared/far depth has already become DEEP, so it naturally disables this shallow-only
// approximation instead of creating flow where terrain/depth data is unavailable.
fn shallow_flow(depth: f32, xz: vec2<f32>) -> vec2<f32> {
    let shallow = smoothstep(0.0, max(wp.coast_fade, 1e-4), depth) *
        (1.0 - smoothstep(max(wp.foam_width, 0.1), max(wp.foam_width * 3.0, 0.3), depth));
    let dxz = dpdx(xz);
    let dyz = dpdy(xz);
    let determinant = dxz.x * dyz.y - dxz.y * dyz.x;
    if (depth >= DEEP * 0.5 || abs(determinant) < 1e-5 || shallow <= 0.0) {
        return vec2<f32>(0.0);
    }
    let ddx = dpdx(depth);
    let ddy = dpdy(depth);
    // Depth increases offshore; carry foam shoreward along the opposite gradient.
    let gradient = vec2<f32>((ddx * dyz.y - ddy * dxz.y) / determinant,
                             (dxz.x * ddy - dyz.x * ddx) / determinant);
    let gradient_length = length(gradient);
    return select(vec2<f32>(0.0), -gradient / gradient_length * shallow * 0.75, gradient_length > 1e-4);
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

// A small shading-only ripple field. Rotating each octave avoids aligned fBm cells;
// exponential shaping preserves fine crest definition without changing displacement.
fn micro_fbm(p: vec2<f32>, t: f32) -> f32 {
    let p0 = p + vec2<f32>(0.11, -0.07) * t;
    let p1 = vec2<f32>(p0.x * 0.81 - p0.y * 0.59, p0.x * 0.59 + p0.y * 0.81);
    let p2 = vec2<f32>(p0.x * 0.36 + p0.y * 0.93, -p0.x * 0.93 + p0.y * 0.36);
    let p3 = vec2<f32>(p0.x * 0.97 - p0.y * 0.24, p0.x * 0.24 + p0.y * 0.97);
    var value = 0.52 * vnoise(p0);
    value = value + 0.27 * vnoise(p1 * 2.07 + vec2<f32>(19.1, 7.3));
    value = value + 0.14 * vnoise(p2 * 4.19 + vec2<f32>(3.7, 31.9));
    value = value + 0.07 * vnoise(p3 * 8.47 + vec2<f32>(43.3, 13.7));
    let signed_value = value * 2.0 - 1.0;
    return 0.5 + 0.5 * sign(signed_value) * pow(abs(signed_value), 1.35);
}

fn micro_normal(xz: vec2<f32>, dist: f32, water_depth: f32, base_normal: vec3<f32>, fft_slope_variance: f32) -> vec3<f32> {
    let p0 = xz * 0.48;
    // Domain warp breaks up the otherwise regular fBm cells without introducing a texture.
    let warp = vec2<f32>(vnoise(p0 * 0.23 + vec2<f32>(11.0, 5.0)),
                         vnoise(p0 * 0.23 + vec2<f32>(37.0, 23.0))) - vec2<f32>(0.5);
    let p = p0 + warp * 0.45;
    let e = 0.035;
    let slope = vec2<f32>(
        micro_fbm(p + vec2<f32>(e, 0.0), wp.time) - micro_fbm(p - vec2<f32>(e, 0.0), wp.time),
        micro_fbm(p + vec2<f32>(0.0, e), wp.time) - micro_fbm(p - vec2<f32>(0.0, e), wp.time)
    ) / (2.0 * e);
    let distance_fade = wave_fade(dist);
    let coast_fade = smoothstep(max(wp.coast_fade, 0.1), max(wp.coast_fade * 3.0, 0.3), water_depth);
    let steep_fade = 1.0 - smoothstep(0.10, 0.38, 1.0 - base_normal.y);
    let fft_roughness_fade = 1.0 - smoothstep(0.035, 0.20, fft_slope_variance);
    let strength = 0.050 * distance_fade * coast_fade * steep_fade * fft_roughness_fade;
    return normalize(vec3<f32>(base_normal.x - slope.x * strength, base_normal.y, base_normal.z - slope.y * strength));
}

// `spec_power` predates the microfacet path. Convert its Blinn-Phong sharpness to
// a conservative perceptual roughness floor, then let the resolved wave variance
// and shading-only ripple perturbation broaden the lobe rather than sharpen it.
fn water_roughness(spec_power: f32, fft_slope_variance: f32, base_normal: vec3<f32>, shading_normal: vec3<f32>) -> f32 {
    let legacy_floor = sqrt(2.0 / max(spec_power + 2.0, 2.0));
    let fft_slope = sqrt(clamp(fft_slope_variance, 0.0, 0.25));
    let micro_slope = length(shading_normal.xz - base_normal.xz);
    return clamp(legacy_floor + fft_slope * 0.26 + micro_slope * 0.35, 0.075, 0.32);
}

fn safe_normalize3(x: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let length_sq = dot(x, x);
    return select(fallback, x * inverseSqrt(max(length_sq, 1e-8)), length_sq > 1e-8);
}

fn schlick_fresnel(f0: f32, cosine: f32) -> f32 {
    return f0 + (1.0 - f0) * pow(max(1.0 - cosine, 0.0), 5.0);
}

fn smith_schlick_g1(ndx: f32, roughness: f32) -> f32 {
    let k = (roughness + 1.0) * (roughness + 1.0) * 0.125;
    return ndx / max(ndx * (1.0 - k) + k, 1e-4);
}

// Equirect lookup into the sky reflection env map. Matches fs_sky_env's convention in sky.wgsl:
// u = azimuth (atan2(z, x)/2pi + 0.5, U-wrapped), v = 0 at zenith .. 1 at nadir (acos(y)/pi).
// `dir` is a world-space direction. Returns linear sky radiance.
fn sky_env_sample(dir: vec3<f32>) -> vec3<f32> {
    let u = 0.5 + atan2(dir.z, dir.x) / TWO_PI;
    let v = acos(clamp(dir.y, -1.0, 1.0)) / (TWO_PI * 0.5);
    return textureSampleLevel(sky_env, sky_env_samp, vec2<f32>(u, v), 0.0).rgb;
}

fn scene_uv(frag_xy: vec2<f32>) -> vec2<f32> {
    return frag_xy / vec2<f32>(textureDimensions(scene_color));
}

struct SceneSample {
    color: vec3<f32>,
    valid: f32,
};

// The distorted lookup is accepted only when opaque geometry is farther from the
// camera than the water surface. Compare reconstructed camera-relative distance,
// not nonlinear reversed-Z values, and keep validity separate from scene colour.
fn refracted_scene(uv: vec2<f32>, surface_rel: vec3<f32>) -> SceneSample {
    let dims = vec2<i32>(textureDimensions(scene_depth));
    let texel = clamp(vec2<i32>(uv * vec2<f32>(dims)), vec2<i32>(0), dims - vec2<i32>(1));
    let opaque_depth = textureLoad(scene_depth, texel, 0);
    if (opaque_depth <= 1e-6) {
        return SceneSample(vec3<f32>(0.0), 0.0);
    }
    let ndc_xy = uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    let opaque_h = frame.inv_view_proj * vec4<f32>(ndc_xy, 1.0 - opaque_depth, 1.0);
    let opaque_rel = opaque_h.xyz / opaque_h.w;
    let behind_water = length(opaque_rel) > length(surface_rel) + 0.05;
    return SceneSample(textureSampleLevel(scene_color, scene_samp, uv, 0.0).rgb, select(0.0, 1.0, behind_water));
}

// Trace the physically reflected view ray against the opaque depth snapshot. This deliberately
// projects every ray position instead of mirroring screen UVs: the latter is not a reflection and
// produces the old angle/distance artifact. A hit must be in front of the ray in reversed-Z and
// reconstruct close to it in camera-relative world space, rejecting foreground and depth-gap leaks.
// Off-screen/missing data falls through to the environment reflection.
fn reflected_scene(surface_rel: vec3<f32>, reflect_dir: vec3<f32>, normal_variation: f32) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(scene_depth));
    var hit_color = vec3<f32>(0.0);
    var hit_weight = 0.0;
    var hit_distance = 0.0;

    // Twenty coarse samples keep this viable in the forward water pass. Terrain and large opaque
    // objects are the intended targets; thin geometry remains an environment-reflection fallback.
    for (var i = 0; i < 20; i = i + 1) {
        if (hit_weight == 0.0) {
            let ray_distance = 2.0 + f32(i) * 10.0;
            let ray_pos = surface_rel + reflect_dir * ray_distance;
            let clip = frame.proj * frame.view * vec4<f32>(ray_pos, 1.0);
            if (clip.w > 1e-5) {
                let ndc = clip.xyz / clip.w;
                let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
                if (all(uv > vec2<f32>(0.001)) && all(uv < vec2<f32>(0.999)) && ndc.z > 0.0 && ndc.z < 1.0) {
                    let texel = clamp(vec2<i32>(uv * vec2<f32>(dims)), vec2<i32>(0), dims - vec2<i32>(1));
                    let opaque_depth = textureLoad(scene_depth, texel, 0);
                    let ray_depth = 1.0 - ndc.z;
                    // Larger reversed depth is closer to the camera. A cleared texel has no scene
                    // intersection, and geometry behind the traced ray must not be reflected.
                    if (opaque_depth > 1e-6 && opaque_depth >= ray_depth) {
                        let opaque_h = frame.inv_view_proj * vec4<f32>(ndc.xy, 1.0 - opaque_depth, 1.0);
                        let opaque_rel = opaque_h.xyz / opaque_h.w;
                        let thickness = 1.5 + ray_distance * 0.025;
                        if (length(opaque_rel - ray_pos) <= thickness) {
                            let edge = min(min(uv.x, uv.y), min(1.0 - uv.x, 1.0 - uv.y));
                            hit_color = textureSampleLevel(scene_color, scene_samp, uv, 0.0).rgb;
                            hit_weight = smoothstep(0.01, 0.07, edge);
                            hit_distance = ray_distance;
                        }
                    }
                }
            }
        }
    }

    // Rapid normal changes are a cheap water-roughness proxy: blur-free SSR is unstable on crests,
    // so leave those pixels to the filtered sky environment instead.
    let roughness_fade = 1.0 - smoothstep(0.08, 0.30, normal_variation);
    let distance_fade = 1.0 - smoothstep(120.0, 192.0, hit_distance);
    return vec4<f32>(hit_color, hit_weight * roughness_fade * distance_fade);
}

// Project the reflected absolute surface point through the mirrored camera. This is
// intentionally not a flipped main-screen UV: parallax follows the reflected camera.
fn planar_reflection(surface_rel: vec3<f32>) -> vec4<f32> {
    if (planar.valid.x < 0.5) { return vec4<f32>(0.0); }
    let absolute = surface_rel + frame.cam_pos.xyz;
    let mirrored = vec3<f32>(absolute.x, 2.0 * wp.sea_level - absolute.y, absolute.z);
    let clip = planar.full_vp * vec4<f32>(mirrored, 1.0);
    if (clip.w <= 1e-5) { return vec4<f32>(0.0); }
    let uv = clip.xy / clip.w * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    let edge = min(min(uv.x, uv.y), min(1.0 - uv.x, 1.0 - uv.y));
    let valid = smoothstep(0.0, 0.03, edge);
    return vec4<f32>(textureSampleLevel(planar_color, planar_samp, clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0).rgb, valid);
}

@fragment
fn fs_water(in: VsOut) -> @location(0) vec4<f32> {
    // Receiver-plane derivatives for the CSM bias must be taken in uniform control flow.
    let dwx = dpdx(in.world_pos);
    let dwy = dpdy(in.world_pos);

    let interaction_cell = 1.0;
    let h_l = interaction_sample(in.base_xz - vec2<f32>(interaction_cell, 0.0)).r;
    let h_r = interaction_sample(in.base_xz + vec2<f32>(interaction_cell, 0.0)).r;
    let h_d = interaction_sample(in.base_xz - vec2<f32>(0.0, interaction_cell)).r;
    let h_u = interaction_sample(in.base_xz + vec2<f32>(0.0, interaction_cell)).r;
    let interaction_normal = normalize(vec3<f32>(h_l - h_r, 2.0 * interaction_cell, h_d - h_u));
    var n = gerstner_normal(in.base_xz, length(in.world_pos));
    if (wp.fft_control.x > 0.5) {
        n = fft_normal(in.base_xz, length(in.world_pos));
    }
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
    let coast_flow = shallow_flow(water_depth, in.base_xz);
    let river_flow = normalize(wp.flow_direction_speed.xy + vec2<f32>(1e-4, 0.0)) *
        max(wp.flow_direction_speed.z, 0.0) * select(0.0, 1.0, wp.flow_direction_speed.w > 0.5);
    let interaction_mask = smoothstep(0.0, wp.coast_fade * 2.0, water_depth);
    let interaction_strength = smoothstep(0.00001, 0.001, abs(h_l) + abs(h_r) + abs(h_d) + abs(h_u));
    n = normalize(mix(n, interaction_normal, interaction_mask * interaction_strength));
    var fft_slope_variance = 0.0;
    var fft_crest = 0.0;
    var fft_compression = 0.0;
    var fft_curvature = 0.0;
    if (wp.fft_control.x > 0.5) {
        for (var layer = 0; layer < 4; layer = layer + 1) {
            let length_m = max(wp.fft_cascade_lengths[layer], 1.0);
            let uv = fract(in.base_xz / length_m);
            let auxiliary = textureSampleLevel(fft_auxiliary, fft_samp, uv, layer, 0.0);
            fft_slope_variance = fft_slope_variance + auxiliary.w;
            fft_crest = max(fft_crest, textureSampleLevel(fft_displacement, fft_samp, uv, layer, 0.0).w);
            fft_compression = max(fft_compression, auxiliary.y);
            fft_curvature = max(fft_curvature, auxiliary.z);
        }
    }
    // Applied after all physical/interactions normals, so micro ripples alter only final shading.
    let base_normal = n;
    n = micro_normal(in.base_xz, length(in.world_pos), water_depth, n, fft_slope_variance);
    let roughness = water_roughness(wp.spec_power, fft_slope_variance, base_normal, n);
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
    let f0 = 0.035;
    // max() guards pow() against a tiny negative base from normalize() rounding (NaN).
    let fresnel = schlick_fresnel(f0, ndv);
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
    let refl_dir = reflect(normalize(in.world_pos), n);
    let normal_variation = length(dpdx(n)) + length(dpdy(n));
    let ssr = reflected_scene(in.world_pos, refl_dir, normal_variation);
    refl = mix(refl, ssr.rgb, ssr.a);
    let planar_refl = planar_reflection(in.world_pos);
    // SSR has the highest-detail on-screen hit; planar fills its off-screen holes,
    // then the atmosphere remains the final fallback.
    refl = mix(refl, planar_refl.rgb, planar_refl.a * (1.0 - ssr.a));
    let uv = scene_uv(in.clip.xy);
    // Refraction is strongest looking down. The normal offset is bounded in pixels so
    // choppy near water cannot pull foreground geometry across the shoreline.
    let refract_uv = clamp(uv + n.xz * (0.002 + 0.010 / (1.0 + water_depth)), vec2<f32>(0.001), vec2<f32>(0.999));
    let refracted = refracted_scene(refract_uv, in.world_pos);
    let transmitted = mix(rgb, refracted.color, refracted.valid * (1.0 - depth_tint) * (1.0 - fresnel));

    // SSR augments, but never replaces, the environment: the snapshot cannot reflect off-screen
    // content or transparent objects, and this renderer has no reflected-camera/clip-plane pass.
    // Slightly boost the physically small water F0 so the sky and valid SSR detail
    // remain legible on the engine's low-contrast terrain palette.
    let reflection_weight = clamp(fresnel * 1.45 + 0.025, 0.0, 1.0);
    rgb = mix(transmitted, refl, reflection_weight);

    // Energy-normalized GGX sunlight. The NDF is broadened by resolved FFT slope
    // variance and micro-ripples, preventing a temporally unstable pin-prick glint.
    // Keep the legacy intensity as a restrained trim: its old default targeted the
    // much lower Blinn-Phong peak, while GGX retains a physically sharper core.
    let h = safe_normalize3(l + v, n);
    let ndh = max(dot(n, h), 0.0);
    let vdh = max(dot(v, h), 0.0);
    let ggx_alpha = roughness * roughness;
    let alpha_sq = ggx_alpha * ggx_alpha;
    let d_base = max(ndh * ndh * (alpha_sq - 1.0) + 1.0, 1e-6);
    let distribution = alpha_sq / (PI * d_base * d_base);
    let visibility = smith_schlick_g1(ndl, roughness) * smith_schlick_g1(ndv, roughness);
    let specular = distribution * visibility * schlick_fresnel(f0, vdh) /
        max(4.0 * ndl * ndv, 1e-4);
    let sun_specular = specular * ndl * wp.spec_intensity * 0.12 * sun_vis;
    rgb = rgb + sun_diffuse * sun_specular;

    // Focused, backlit crests transmit a little direct sunlight. Curvature and
    // horizontal compression reject broad swells; sufficient column depth rejects
    // the shoreline foam band. This is direct-light scattering, not emission.
    let crest_shape = smoothstep(0.025, 0.09, fft_crest) *
        smoothstep(0.008, 0.030, fft_compression) *
        smoothstep(0.0015, 0.014, fft_curvature);
    let view_xz = safe_normalize3(vec3<f32>(v.x, 0.0, v.z), vec3<f32>(0.0, 0.0, 1.0)).xz;
    let light_xz = safe_normalize3(vec3<f32>(l.x, 0.0, l.z), vec3<f32>(0.0, 0.0, -1.0)).xz;
    let backlit = smoothstep(0.10, 0.70, dot(view_xz, -light_xz));
    let crest_depth = smoothstep(0.35, 1.5, min(water_depth, DEEP));
    let crest_scatter = crest_shape * backlit * crest_depth * sun_vis * 0.035;
    rgb = rgb + sun_diffuse * crest_scatter;

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
    // The depth field already includes displaced water height. Use the local crest to
    // concentrate the otherwise restrained shore foam at wave arrivals on the beach.
    let surface_wave = in.world_pos.y + frame.cam_pos.y - wp.sea_level;
    let shore_break = smoothstep(0.015, 0.12, surface_wave);

    // Shoreline foam: churning procedural noise in a band along the (swash-moved) edge. The band
    // fades IN from the waterline and OUT into deeper water (peak ~1/4 of foam_width in), so the
    // LAND side dissolves softly instead of ending on a hard bright line where the water geometry
    // is clipped by the beach — the deep side already faded. Foam also fades to 0 right at the
    // edge, so the water there is transparent (soft wash over wet sand) rather than opaque white.
    let ft = eff_depth / max(wp.foam_width, 1e-4);
    let foam_band = smoothstep(0.0, 0.25, ft) * (1.0 - smoothstep(0.25, 1.0, ft));
    let foam = clamp(foam_band * foam_noise(in.base_xz + (coast_flow + river_flow) * wp.time, wp.time) * wp.foam_intensity * (1.0 + shore_break * 0.75), 0.0, 1.0);
    let foam_history_sample = persistent_foam_sample(in.base_xz);
    // The compute source is thresholded, so calm water with no events or breaking crests remains clean.
    let persistent_foam = clamp(foam_history_sample.r * (0.52 + foam_history_sample.b * 0.30), 0.0, 1.0);
    let combined_foam = max(foam, persistent_foam);
    // Bubbles form a broad, rough dielectric layer: mostly sky/sun-lit diffuse scattering with
    // only a subdued dielectric highlight. This is material response, never an emissive overlay.
    var foam_color = vec3<f32>(0.0);
    if (combined_foam > 0.0) {
        let foam_normal = normalize(mix(n, vec3<f32>(0.0, 1.0, 0.0), 0.72));
        let foam_ndl = max(dot(foam_normal, l), 0.0);
        let foam_ndv = max(dot(foam_normal, v), 0.0);
        let foam_h = safe_normalize3(l + v, foam_normal);
        let foam_ndh = max(dot(foam_normal, foam_h), 0.0);
        let foam_vdh = max(dot(v, foam_h), 0.0);
        let foam_roughness = 0.72;
        let foam_alpha_sq = pow(foam_roughness, 4.0);
        let foam_d_base = max(foam_ndh * foam_ndh * (foam_alpha_sq - 1.0) + 1.0, 1e-6);
        let foam_distribution = foam_alpha_sq / (PI * foam_d_base * foam_d_base);
        let foam_visibility = smith_schlick_g1(foam_ndl, foam_roughness) * smith_schlick_g1(foam_ndv, foam_roughness);
        let foam_specular = foam_distribution * foam_visibility * schlick_fresnel(0.02, foam_vdh) /
            max(4.0 * foam_ndl * foam_ndv, 1e-4) * foam_ndl * sun_vis;
        let foam_albedo = vec3<f32>(0.88, 0.92, 0.94);
        let foam_diffuse = (sun_ambient * amb_ao + sun_diffuse * foam_ndl * sun_vis) * (foam_albedo / PI);
        foam_color = foam_diffuse + sun_diffuse * foam_specular * 0.08;
    }
    rgb = mix(rgb, foam_color, combined_foam);

    // Base opacity, Fresnel-opaque at grazing angles (where real water is a mirror) so
    // the seabed only shows through looking down. A soft shoreline fade dissolves the water
    // to transparent as the (swash-moved) column depth -> 0, so the hard coast cut becomes a
    // gentle wash over the visible wet beach; foam forces opacity where it sits.
    let shore = smoothstep(0.0, wp.coast_fade, eff_depth);
    let alpha = max(mix(wp.alpha, 1.0, fresnel) * shore, combined_foam);
    return vec4<f32>(rgb, alpha);
}
