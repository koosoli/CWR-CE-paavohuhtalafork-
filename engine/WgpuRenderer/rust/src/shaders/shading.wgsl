// Shared object fragment shading — extracted verbatim from shader3d.wgsl's fs_main so the
// GPU-driven indirect path (docs/gpu-culling-and-depth-plan.md Stage 3) reuses the EXACT
// same lit look. Given a sampled albedo + the six resolved material colour terms, produces
// the final fogged rgb. The per-draw path folds the sun into the material CPU-side and
// passes the folded terms; the GPU-driven path folds raw material × the frame sun in the
// shader — both then call shade(), so the lighting / shadow / specular / fog logic lives in
// ONE place and can't drift between the two paths.

#define_import_path shading

#import frame::{frame, terrain_sun_shadow, apply_fog, sky_irradiance}
#import shadow::shadow_strength
#import lighting::lights_contrib
#import color::srgb_to_linear

// The six material colour terms shade() consumes (raw gamma-space; shade srgb-decodes on
// the HDR/linear path). sun_ambient/sun_diffuse are the SUN-FOLDED terms (material × sun),
// used only on the legacy path; light_diffuse/light_ambient are the raw material terms for
// the frame-global local lights; specular is the sun-folded highlight colour and spec_power
// its exponent.
struct ShadeMaterial {
    emissive: vec3<f32>,
    sun_ambient: vec3<f32>,
    sun_diffuse: vec3<f32>,
    light_diffuse: vec3<f32>,
    light_ambient: vec3<f32>,
    specular: vec3<f32>,
    spec_power: f32,
};

// world_pos is camera-relative; dwx/dwy = dpdx/dpdy(world_pos), computed in the entry point
// under uniform control flow (before any discard). `linear` selects the HDR path,
// `is_cutout` the alpha-tested-foliage canopy-AO branch, `foliage_shadow_ao` its constant.
fn shade(
    albedo_in: vec3<f32>,
    mat: ShadeMaterial,
    normal: vec3<f32>,
    world_pos: vec3<f32>,
    fog: f32,
    dwx: vec3<f32>,
    dwy: vec3<f32>,
    linear: f32,
    foliage_shadow_ao: f32,
    is_cutout: bool,
    // Alpha-blended (glass) surface: damp the diffuse sky-irradiance ambient (a transparent
    // canopy is not a diffuse reflector; a full sky wash blows it out + spikes auto-exposure).
    is_translucent: bool,
) -> vec3<f32> {
    var albedo = albedo_in;
    var m_emissive = mat.emissive;
    var m_sun_ambient = mat.sun_ambient;
    var m_sun_diffuse = mat.sun_diffuse;
    var m_light_diffuse = mat.light_diffuse;
    var m_light_ambient = mat.light_ambient;
    var m_specular = mat.specular;
    if (linear > 0.5) {
        albedo = srgb_to_linear(albedo);
        m_emissive = srgb_to_linear(m_emissive);
        m_sun_ambient = srgb_to_linear(m_sun_ambient);
        m_sun_diffuse = srgb_to_linear(m_sun_diffuse);
        m_light_diffuse = srgb_to_linear(m_light_diffuse);
        m_light_ambient = srgb_to_linear(m_light_ambient);
        m_specular = srgb_to_linear(m_specular);
    }
    let nrm = normalize(normal);
    let ndotl = max(dot(nrm, -frame.sun_dir_world.xyz), 0.0);
    let sky_lit = frame.sun_diffuse.w > 0.5;
    // CSM shadow (near contact). Folded into the sun removal on the sky-lit path, kept as a
    // final multiply on the legacy path — exactly as the original fs_main did.
    let csm_s = shadow_strength(world_pos, nrm, fog, dwx, dwy);
    // Long-range terrain sun-shadow: removes direct sun (diffuse+specular), keeps ambient/
    // emissive/local, sampled by absolute world position (world_pos is camera-relative).
    let world_abs = world_pos + frame.cam_pos.xyz;
    let terrain_s = terrain_sun_shadow(world_abs.xz, world_abs.y);
    let sun_occ = select(terrain_s, max(terrain_s, csm_s), sky_lit);
    let sun_vis = 1.0 - sun_occ;
    var sun: vec3<f32>;
    if (sky_lit) {
        // Sky-based lighting: frame-global atmosphere sun + DIRECTIONAL sky-irradiance ambient
        // (SH-9 projection of the env map, evaluated per normal), scaled by the skyAmbient knob in
        // sun_ambient.w. albedo is the reflectance via `rgb = albedo * lit`. The per-material folded
        // sun (m_sun_*) is deliberately unused here — see the original fs_main note.
        var ambient = sky_irradiance(nrm) * frame.sun_ambient.w;
        // Glass canopies: keep only a fraction of the sky wash so they read as glazing, not a lit
        // diffuse dome (the direct sun sheen + any glint still sit on top).
        if (is_translucent) {
            ambient *= 0.2;
        }
        sun = m_emissive + ambient + frame.sun_diffuse.rgb * ndotl * sun_vis;
    } else {
        sun = m_emissive + m_sun_ambient + m_sun_diffuse * ndotl * sun_vis;
    }
    let local = lights_contrib(world_pos, nrm, m_light_diffuse, m_light_ambient, linear);
    let raw = sun + local;
    let lit = select(clamp(raw, vec3<f32>(0.0), vec3<f32>(1.0)), max(raw, vec3<f32>(0.0)), linear > 0.5);
    var rgb = albedo * lit;
    // Sun-only Blinn-Phong specular (untextured, additive, before the shadow multiply). The
    // camera is at the origin in camera-relative space, so view_dir = normalize(-world_pos).
    if (mat.spec_power > 0.0) {
        let view_dir = normalize(-world_pos);
        let half_vec = normalize(-frame.sun_dir_world.xyz + view_dir);
        let n_dot_h = max(dot(nrm, half_vec), 0.0);
        let spec = m_specular * pow(n_dot_h, max(mat.spec_power, 1.0));
        let spec_vis = spec * sun_vis;
        rgb += select(clamp(spec_vis, vec3<f32>(0.0), vec3<f32>(1.0)), max(spec_vis, vec3<f32>(0.0)), linear > 0.5);
    }
    // Canopy self-occlusion for alpha-tested foliage in terrain shadow (terrain-shadow only;
    // CSM already darkens near foliage).
    if (is_cutout) {
        rgb *= mix(1.0, foliage_shadow_ao, terrain_s);
    }
    // Legacy path keeps CSM as a final colour multiply; the sky-lit path already removed the
    // direct sun in shadow above, so it must not double-darken here.
    if (!sky_lit) {
        rgb *= mix(1.0, frame.shadow.ctlb.y, csm_s);
    }
    var fog_color = frame.fog_color.rgb;
    if (linear > 0.5) {
        fog_color = srgb_to_linear(fog_color);
    }
    // fog_enabled: >=2 = aerial-perspective froxel (per-fragment); 1 = legacy flat fog; 0 =
    // off (fog == 1, mix is a no-op).
    if (frame.params.fog_enabled >= 1.5) {
        rgb = apply_fog(rgb, world_pos);
    } else {
        rgb = mix(fog_color, rgb, fog);
    }
    return rgb;
}
