#define_import_path frame

// Shared group(0): the per-frame camera/environment UBO plus the cascade shadow
// map + comparison sampler. Every 3D pipeline (lit meshes, terrain) binds this
// exact layout, so the structs and bindings live here once. The UBO global is
// deliberately named `frame` so importers can write `frame.proj` etc. unchanged.

struct FrameParams {
    fog_start: f32,
    fog_inv_range: f32,
    fog_enabled: f32, // 0 = off, 1 = on
    shadow_strength: f32,
};

struct ShadowBlock {
    cascade_vp: array<mat4x4<f32>, 4>,
    splits: vec4<f32>, // per-tier select distance (omni radius / frustum eye-depth)
    omni_radius: vec4<f32>,
    ctl: vec4<f32>,  // {count, omni_count, fade_range, bias_const}
    // `ctlb`, not `ctl2`: naga_oil forbids composable-module identifiers ending
    // in a digit (naga's namer reserves numeric suffixes for disambiguation).
    ctlb: vec4<f32>, // {texel_size, darkness, normal_offset_scale, pcf}
    cam_fwd: vec4<f32>,
    sun_dir: vec4<f32>,
};

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    params: FrameParams,
    shadow: ShadowBlock,
    cam_pos: vec4<f32>, // world-space camera position (used by the terrain pipeline)
    sun_diffuse: vec4<f32>, // sun light, accommodation folded in (terrain)
    sun_ambient: vec4<f32>,
    sun_dir_world: vec4<f32>, // main light's surface-to-light direction (terrain)
    // inverse(view) * inverse(proj), computed Rust-side in f64 (the reversed-Z / infinite-far
    // proj is ill-conditioned to invert in f32; invert the two SEPARATELY, as the sky does).
    // Unprojects a forward-NDC point vec4(ndc.xy, 1 - stored_depth, 1) to a CAMERA-RELATIVE
    // world position (÷ w). Appended after the WgrCamera bytes in the camera upload, so it is
    // NOT part of the WgrCamera C ABI. Used by water seabed-depth reconstruction (Stage 2);
    // reusable by SSAO / refraction / contact shadows.
    inv_view_proj: mat4x4<f32>,
    // Foliage lighting knobs (docs/foliage-translucency-plan.md), appended after inv_view_proj
    // in the camera upload (mirrors WgrFoliage). Read by shade() for cutout/vegetation draws.
    //   foliage  = (trans_scale, distortion, trans_power, wrap)
    //   foliageb = (ambient_boost, normal_bend[bush], crown_y_offset[bush], fill_fade_end)
    //   foliagec = (gi_strength, tree_bend, tree_crown_y, _)
    // `foliageb`/`foliagec` not `foliage2`/`foliage3`: naga_oil forbids composable identifiers
    // ending in a digit.
    foliage: vec4<f32>,
    foliageb: vec4<f32>,
    foliagec: vec4<f32>,
    // xyz = plane normal, w = offset; zero normal disables clipping. This is appended
    // by Rust, keeping WgrCamera's C++ ABI unchanged.
    clip_plane: vec4<f32>,
    // Screen-space AO gate (docs/screen-space-ao-plan.md), appended after clip_plane.
    //   x = 1 when the GTAO pass ran this frame and its buffer may be read (0 = off)
    //   y = 1 for the raw AO debug view
    //   z = 1 to steer sky irradiance by the bent normal (Stage 2 directional ambient)
    //   w = unused
    gtao: vec4<f32>,
};

// One frame-global point or spot light. Positions are ABSOLUTE world space so a
// single upload serves every camera; the shader reconstructs the camera-relative
// offset via frame.cam_pos. Colours are pre-scaled by NightEffect on the CPU
// (fade out by day). Matches the C ABI WgrLight.
struct Light {
    pos: vec4<f32>,     // xyz = world-absolute position, w = start-attenuation distance
    diffuse: vec4<f32>, // rgb = diffuse * nightEffect
    ambient: vec4<f32>, // rgb = ambient * nightEffect
    dir: vec4<f32>,     // xyz = beam direction (spot), w = isSpot (1) else 0
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d_array;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
// Frame-global light store, shared by the lit-mesh + terrain pipelines. The
// active light count for this camera rides in frame.cam_pos.w (the buffer itself
// is a fixed capacity, so its length is not the count).
@group(0) @binding(3) var<storage, read> lights: array<Light>;

// Long-range terrain sun-shadow mask (terrain_shadow.wgsl compute sweep), promoted
// into the shared frame group so BOTH terrain and lit meshes sample it — that is
// how objects (infantry, buildings, aircraft) receive a mountain's cast shadow, not
// just the terrain. Per column it stores the world height below which that column is
// terrain-shadowed: .r = ceiling, .g = penumbra half-width (m), .b = strength; a
// point at (xz, y) is occluded by how far y sits below the ceiling. `map` takes a
// world-xz to the mask's [0,1] UV; enabled = 0 until a heightmap is loaded.
struct TerrainShadowMap {
    origin: vec2<f32>,     // world_origin
    inv_span: vec2<f32>,   // 1 / (hm_dims * terrain_grid): world-xz -> [0,1] over the map
    half_texel: vec2<f32>, // 0.5 / mask_dims
    enabled: f32,
    // Sky-visibility (AO) controls (docs/sky-visibility-ambient-plan.md): strength scales the effect
    // (0 = off), floor keeps a minimum ambient in fully-occluded columns.
    sky_vis_strength: f32,
    sky_vis_floor: f32,
    sky_vis_debug: f32,    // 1 = terrain outputs the raw sky-view factor as greyscale
    sky_vis_contrast: f32, // occ = 1 - pow(V, contrast); >1 deepens the AO for near-1 V
    // naga_oil forbids composable identifiers ending in a digit (see `ctlb` above), so pad_c.
    pad_c: f32,
};
@group(0) @binding(4) var terrain_shadow_mask: texture_2d<f32>;
@group(0) @binding(5) var terrain_shadow_samp: sampler;
@group(0) @binding(6) var<uniform> terrain_shadow_map: TerrainShadowMap;

// Aerial-perspective froxel volume (filled by cs_froxel in sky.wgsl): XY = screen,
// Z = distance with a squared distribution. rgb = in-scattered light toward the camera,
// a = transmittance from the camera to that froxel. The forward shaders apply fog with
// ONE trilinear tap here — every fragment fogs by its OWN distance, so transparents and
// foliage are correct and 2D (which never samples this) is simply never fogged. The
// clamping sampler is shared with the terrain mask (binding 5).
@group(0) @binding(7) var froxel_tex: texture_3d<f32>;
@group(0) @binding(8) var froxel_samp: sampler;

// Diffuse sky irradiance as 9 spherical-harmonic RGB coefficients, projected from the sky
// reflection env map each frame (sky_sh.wgsl / Sky::render_sh). The lit-mesh + terrain fragment
// shaders evaluate directional ambient from these on the sky-lit path (sky_irradiance), replacing
// the old flat ambient fill. rgb per coeff; w padding.
struct SkySh {
    c: array<vec4<f32>, 9>,
};
@group(0) @binding(9) var<uniform> sky_sh: SkySh;

// Coarse sky-visibility (sky-view factor) mask: R8Unorm, one bilinear tap gives the cosine-weighted
// fraction of sky a terrain column can see (1 = open, 0 = fully occluded). Terrain-owned, produced by
// the CPU horizon scan (terrain/skyvis.rs); sampled with the terrain-shadow sampler (binding 5) and
// mapping (binding 6). See terrain_sky_visibility / sky_vis_ao below.
@group(0) @binding(10) var terrain_skyvis_mask: texture_2d<f32>;

// Screen-space ambient occlusion (GTAO + bilateral blur, gfx3d/gtao*.wgsl) at render resolution:
//   rgb = bent normal, VIEW space, unit length — the average direction light still reaches this
//         pixel from (Stage 2)
//   a   = ambient visibility in [0,1], 1 = unoccluded
// Gfx3d-owned, produced from the depth+normal prepass each frame before the colour pass. Read
// with textureLoad at the fragment's OWN pixel — it is already a per-pixel screen-space quantity,
// and under MSAA every covered sample of a pixel legitimately shares one value (plan §5).
// See gtao_ao / gtao_bent_normal_world below.
@group(0) @binding(11) var gtao_tex: texture_2d<f32>;

// Diffuse sky irradiance for a world-space surface normal, from the SH-9 sky projection
// (Ramamoorthi, "An Efficient Representation for Irradiance Environment Maps"), divided by PI so
// it is the Lambertian ambient reflectance factor (final ambient = albedo * sky_irradiance * scale).
// A future sky-visibility (AO) term multiplies this per point by the fraction of sky it can see.
fn sky_irradiance(n: vec3<f32>) -> vec3<f32> {
    let l00 = sky_sh.c[0].rgb;
    let l1m1 = sky_sh.c[1].rgb;
    let l10 = sky_sh.c[2].rgb;
    let l11 = sky_sh.c[3].rgb;
    let l2m2 = sky_sh.c[4].rgb;
    let l2m1 = sky_sh.c[5].rgb;
    let l20 = sky_sh.c[6].rgb;
    let l21 = sky_sh.c[7].rgb;
    let l22 = sky_sh.c[8].rgb;
    let x = n.x;
    let y = n.y;
    let z = n.z;
    let c1 = 0.429043;
    let c2 = 0.511664;
    let c3 = 0.743125;
    let c4 = 0.886227;
    let c5 = 0.247708;
    let e = c1 * l22 * (x * x - y * y) + c3 * l20 * (z * z) + c4 * l00 - c5 * l20
        + 2.0 * c1 * (l2m2 * x * y + l21 * x * z + l2m1 * y * z)
        + 2.0 * c2 * (l11 * x + l1m1 * y + l10 * z);
    return max(e, vec3<f32>(0.0)) * (1.0 / 3.14159265359);
}

// Aerial-perspective fog for a camera-relative fragment position. The froxel volume
// supplies only the fog COLOUR — the physically scattered airlight along this view ray,
// so it reddens toward the sun and meets the sky seamlessly at the horizon. The blend
// AMOUNT comes from the scene fog range (fogStart -> fogMax), NOT the froxel's physical
// transmittance: over the game's short (~km) view distance physical extinction is far
// too weak to dissolve distant geometry (objects would pop at the cull edge) and its
// inscatter is far too bright up close (uniform grey wash, since the airlight is on the
// sky's physical radiance scale while surfaces are legacy-lit ~1). Gating by the game
// fog factor fixes both by construction: near -> amount 0 -> untouched surface; at fogMax
// -> amount 1 -> surface fully replaced by the airlight, dissolving into the sky. The
// engine widens/narrows [fogStart, fogMax] per weather, so THAT is the atmosphere-density
// / "not every morning is foggy" control. Per-fragment: foliage, fences and transparents
// all fog by their own pixel distance; 2D never calls this.
fn apply_fog(rgb: vec3<f32>, world_pos_rel: vec3<f32>) -> vec3<f32> {
    if (frame.params.fog_enabled <= 0.5 || frame.params.fog_inv_range <= 0.0) {
        return rgb;
    }
    let dist = length(world_pos_rel);
    // max_dist = fogStart + 1/fogInvRange = the scene fog-max range, which the engine also
    // uses as the camera far plane AND the terrain-grid cull distance — i.e. the real max
    // draw distance. Geometry cannot exist past it, so it's the anchor: at max_dist the fog
    // is full, so a terrain tile appearing at the far clip is already fully dissolved into
    // the sky (see cs_froxel: the far froxels ARE the sky) and fades in smoothly as it nears.
    let max_dist = frame.params.fog_start + 1.0 / frame.params.fog_inv_range;
    // Exponential (power) ramp, replacing the game's broad linear fogStart->fogMax fade:
    // pow(u, k) is ~0 across the near/mid field and rises hard only near the edge, so the
    // scene stays clear yet nothing pops at the cull distance. Density tracks the weather
    // fog range via max_dist (shorter range in fog -> u climbs sooner -> heavier fog), so
    // this stays weather-responsive with no gameplay impact (game fog logic is untouched).
    // Falloff exponent (frame.fog_color.w, from SkySettings::fogFalloff): high = clear near/
    // mid + fog only at the edge; low (~1) = dense fog throughout, revealing the froxel's
    // volumetric terrain sun-shadowing / god rays. Guarded so a zero-fill can't full-fog.
    let falloff = max(frame.fog_color.w, 0.1);
    let u = clamp(dist / max_dist, 0.0, 1.0);
    let amount = pow(u, falloff);
    // Screen uv from a reprojection of the world position (matches cs_froxel's ndc->uv),
    // and distance -> slice with the fill's squared map (w = sqrt(dist / max)).
    let clip = frame.proj * frame.view * vec4<f32>(world_pos_rel, 1.0);
    let uv = (clip.xy / clip.w) * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5);
    let inscat = textureSampleLevel(froxel_tex, froxel_samp, vec3<f32>(uv, sqrt(u)), 0.0).rgb;
    return mix(rgb, inscat, amount);
}

// Occlusion [0,1] of the sun by terrain at world position (xz, y): 0 = lit, 1 =
// fully in terrain shadow. Zero when the feature is off or the point is off the map
// or above the shadow ceiling. Shared by the terrain and lit-mesh fragment shaders.
fn terrain_sun_shadow(world_xz: vec2<f32>, world_y: f32) -> f32 {
    if (terrain_shadow_map.enabled < 0.5) {
        return 0.0;
    }
    let uv = (world_xz - terrain_shadow_map.origin) * terrain_shadow_map.inv_span
             + terrain_shadow_map.half_texel;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return 0.0;
    }
    let sm = textureSampleLevel(terrain_shadow_mask, terrain_shadow_samp, uv, 0.0);
    let lit = smoothstep(sm.r - sm.g, sm.r + sm.g + 1e-3, world_y);
    return clamp(sm.b * (1.0 - lit), 0.0, 1.0);
}

// Cosine-weighted fraction of sky a terrain column can see [0,1] (1 = open sky). Position-only
// (evaluated at the terrain surface); the AMBIENT-occlusion analogue of terrain_sun_shadow, which
// occludes the DIRECT sun. Returns 1 (full sky) off-map or before a heightmap loads so absence of
// data never darkens. Reuses the terrain-shadow mapping/sampler (no half-texel offset: the coarse,
// smooth mask relies on the clamping sampler at the edges).
fn terrain_sky_visibility(world_xz: vec2<f32>) -> f32 {
    if (terrain_shadow_map.enabled < 0.5) {
        return 1.0;
    }
    let uv = (world_xz - terrain_shadow_map.origin) * terrain_shadow_map.inv_span;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return 1.0;
    }
    return textureSampleLevel(terrain_skyvis_mask, terrain_shadow_samp, uv, 0.0).r;
}

// Ambient-occlusion multiplier for the sky ambient term at a terrain column: blends from 1 (no AO)
// toward the sky-view factor by sky_vis_strength, then floors it so occluded ground never goes fully
// black. strength = 0 -> returns 1 (feature off). Multiply the sky ambient term by this.
fn sky_vis_ao(world_xz: vec2<f32>) -> f32 {
    let v = clamp(terrain_sky_visibility(world_xz), 0.0, 1.0);
    // Occlusion, contrast-shaped so the near-1 V of smooth heightfields still darkens visibly:
    // contrast = 1 -> occ = 1 - V (linear); contrast > 1 -> pow(V, contrast) < V -> deeper occ.
    let occ = 1.0 - pow(v, terrain_shadow_map.sky_vis_contrast);
    let ao = 1.0 - terrain_shadow_map.sky_vis_strength * occ;
    return max(ao, terrain_shadow_map.sky_vis_floor);
}

// Screen-space AO multiplier at this fragment [0,1], 1 = unoccluded. Returns 1 when the pass did
// not run: the AO texture RETAINS its last contents when GTAO is disabled or skipped, so an
// ungated read would shade the world with a frozen AO buffer — a failure that looks like a
// lighting bug rather than a missing pass. Multiply into the AMBIENT term only (plan §6): AO on
// direct sun is the classic over-darkening artifact, and direct occlusion is the shadow maps' job.
fn gtao_ao(frag_coord: vec2<f32>) -> f32 {
    if (frame.gtao.x < 0.5) {
        return 1.0;
    }
    let px = vec2<i32>(frag_coord);
    let dims = vec2<i32>(textureDimensions(gtao_tex));
    let q = clamp(px, vec2<i32>(0), dims - vec2<i32>(1));
    return clamp(textureLoad(gtao_tex, q, 0).a, 0.0, 1.0);
}

// The bent normal in WORLD space, or `fallback` (the geometric normal) when GTAO is off or the
// feature is disabled. This is the Stage-2 payload: sampling sky irradiance along the direction
// light actually arrives from, rather than along the surface normal, is what gives a shaded
// surface near an occluder some form instead of a flat wash.
//
// GTAO works in VIEW space, so rotate back. frame.view has its translation zeroed and is
// otherwise a rotation, so its inverse is its transpose — which `v * M` computes in WGSL
// (row-vector convention), avoiding an explicit inverse.
//
// frame.gtao.z gates it separately from gtao.x: the AO term is worth having on its own, and the
// directional ambient is the part most likely to need backing out if it looks wrong.
fn gtao_bent_normal_world(frag_coord: vec2<f32>, fallback: vec3<f32>) -> vec3<f32> {
    if (frame.gtao.x < 0.5 || frame.gtao.z < 0.5) {
        return fallback;
    }
    let px = vec2<i32>(frag_coord);
    let dims = vec2<i32>(textureDimensions(gtao_tex));
    let q = clamp(px, vec2<i32>(0), dims - vec2<i32>(1));
    let bent_view = textureLoad(gtao_tex, q, 0).xyz;
    if (dot(bent_view, bent_view) < 1e-6) {
        return fallback;
    }
    let bent_world = (vec4<f32>(normalize(bent_view), 0.0) * frame.view).xyz;
    return normalize(bent_world);
}

// Raw GTAO debug view mode: 0 = off, 1 = AO as greyscale, 2 = bent normal as RGB. Shipped WITH
// the effect, not after it: judging AO through a full lighting pipeline — sun, SH ambient, fog,
// tonemap — is much harder than looking at the buffer itself.
//
// Mode 2 exists because mode 1 shows only the scalar term, so the bent normal was invisible to
// inspection: toggling directional ambient changed nothing in the debug view and everything in
// the lit one, which is a confusing way to evaluate a feature.
fn gtao_debug_mode() -> f32 {
    return frame.gtao.y;
}

fn gtao_debug_on() -> f32 {
    return select(0.0, 1.0, frame.gtao.y > 0.5);
}

// What the debug view should draw at this pixel: greyscale AO, or the bent normal mapped from
// [-1,1] to [0,1] so directions read as colour.
fn gtao_debug_colour(frag_coord: vec2<f32>, fallback_n: vec3<f32>) -> vec3<f32> {
    if (frame.gtao.y > 1.5) {
        return gtao_bent_normal_world(frag_coord, fallback_n) * 0.5 + vec3<f32>(0.5);
    }
    return vec3<f32>(gtao_ao(frag_coord));
}

// 1 when the sky-visibility debug view is on (terrain shows the factor as greyscale). A helper
// so importers need not reference the terrain_shadow_map global directly.
fn sky_vis_debug_on() -> f32 {
    return terrain_shadow_map.sky_vis_debug;
}

// Contrast-shaped visibility for the debug view: pow(V, contrast). Unlike raw V (which sits near 1 on
// smooth terrain and reads as flat white), this pulls the factor down by the same contrast the AO
// uses, so the mask shape — and its response to radius/azimuths/downsample/contrast — is legible.
fn sky_vis_debug_value(world_xz: vec2<f32>) -> f32 {
    let v = clamp(terrain_sky_visibility(world_xz), 0.0, 1.0);
    return pow(v, terrain_shadow_map.sky_vis_contrast);
}

// Reversed-Z: the shared projection is forward (near->0, far->1). Remap to
// near->1, far->0 so the float depth buffer spends its exponent bits where
// geometry actually is (far from 0), which massively improves precision at range
// vs forward float depth. Pipelines use GreaterEqual + clear-to-0.
fn reverse_z(clip: vec4<f32>) -> vec4<f32> {
    var c = clip;
    c.z = c.w - c.z;
    return c;
}

// Scene fog blend factor in [0,1] for a camera distance: 1 = keep colour, 0 =
// full fog. Returns 1 unconditionally when fog is disabled.
fn fog_factor(dist: f32) -> f32 {
    let f = clamp(1.0 - (dist - frame.params.fog_start) * frame.params.fog_inv_range, 0.0, 1.0);
    return select(1.0, f, frame.params.fog_enabled > 0.5);
}
