// Unlit textured 3D, plain (vs_main) and GPU-skinned (vs_skinned) variants
// sharing fs_main. The two group(1) declarations coexist because each entry
// point statically uses only one (binding collisions are validated per entry
// point): the plain pipeline binds `object`, the skinned pipeline binds the
// bone `palette` (from the skin module). Shadowing is the shared cascade kernel.

#import frame::{frame, reverse_z, fog_factor}
#import shadow::shadow_strength
#import skin::{skin_pos, skin_normal}
#import lighting::lights_contrib

struct Object {
    world: mat4x4<f32>,
};

// Per-draw material lighting, folded on the CPU exactly like GL33's
// UploadVSMaterialConstants (raw sun colour x material, sun-enable already in the
// sun terms). Bound at group(1)/binding(1) for BOTH the plain and skinned
// pipelines — binding(0) is `object` (plain) or the skin module's `palette`
// (skinned), so the material coexists with either. Only rgb is read.
struct Material {
    emissive: vec4<f32>,
    sun_ambient: vec4<f32>,
    sun_diffuse: vec4<f32>,
    // Modulation for the frame-global point/spot lights (GL33's matDif/matAmb).
    light_diffuse: vec4<f32>,
    light_ambient: vec4<f32>,
};

@group(1) @binding(0) var<uniform> object: Object;   // plain pipeline
@group(1) @binding(1) var<uniform> material: Material;
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

// Baked per-pipeline (pipeline-overridable constants): the alpha-test cutout
// threshold and whether this is a shadow-darken pipeline. Keeping them out of a
// per-draw binding avoids a 5th bind group (wgpu's default maxBindGroups is 4).
override alpha_ref: f32 = 0.0;   // discard fragments with alpha below this (0 = off)
override is_shadow: f32 = 0.0;   // 1 = output black + shadow-strength alpha
// Decal/overlay depth bias in reversed-NDC depth units, pulling the draw toward the
// camera. Applied in the vertex shader (not DepthBiasState, which is a no-op on the
// float depth format this backend gets) so roads/decals/overlays win the depth test
// against coplanar geometry.
override depth_bias: f32 = 0.0;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fog: f32,             // 1 = keep colour, 0 = full fog
    @location(2) world_pos: vec3<f32>, // camera-relative
    @location(3) normal: vec3<f32>,    // world space, outward
};

// world_pos is camera-relative (the world matrix / palette is offset by the
// camera position on the C++ side), so its length is the camera distance.
fn finish_vertex(world_pos: vec4<f32>, normal_ws: vec3<f32>, uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * world_pos);
    // Bias toward the camera (larger reversed depth) so decals/overlays win the
    // depth test against coplanar geometry.
    out.clip.z += depth_bias * out.clip.w;
    out.uv = uv;
    out.world_pos = world_pos.xyz;
    out.normal = normal_ws;
    out.fog = fog_factor(length(world_pos.xyz));
    return out;
}

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VsOut {
    let world_pos = object.world * vec4<f32>(pos, 1.0);
    // Vertex normals are uploaded negated (D3D convention); un-negate for the
    // outward world normal.
    let rot = mat3x3<f32>(object.world[0].xyz, object.world[1].xyz, object.world[2].xyz);
    return finish_vertex(world_pos, -(rot * norm), uv);
}

// Linear-blend skinning (see the skin module). Vertices with no skin weight
// carry a single weight of 1.0 on a reserved bone whose palette entry is just
// `world`, so no zero-weight fallback is needed.
@vertex
fn vs_skinned(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bones: vec4<u32>,   // Uint8x4: palette indices
    @location(4) weights: vec4<f32>, // Unorm8x4: normalised weights
) -> VsOut {
    let world_pos = skin_pos(pos, bones, weights);
    let normal_ws = skin_normal(norm, bones, weights);
    return finish_vertex(world_pos, -normal_ws, uv);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Derivatives must run in uniform control flow — before the discard.
    let dwx = dpdx(in.world_pos);
    let dwy = dpdy(in.world_pos);
    let base = textureSample(tex, samp, in.uv);
    if (base.a < alpha_ref) {
        discard;
    }
    if (is_shadow > 0.5) {
        return vec4<f32>(0.0, 0.0, 0.0, frame.params.shadow_strength * base.a);
    }
    // Per-material sun lighting plus frame-global point/spot lights, matching
    // GL33's VSNormal but per-fragment: clamp(emissive + sun_ambient +
    // sun_diffuse * N.L + SUM(local lights), 0, 1), then x texture (the
    // `vColor * tex0` of PSNormal). sun_dir_world is the light travel direction,
    // dotted against its negation like the sun term there.
    let nrm = normalize(in.normal);
    let ndotl = max(dot(nrm, -frame.sun_dir_world.xyz), 0.0);
    let sun = material.emissive.rgb + material.sun_ambient.rgb + material.sun_diffuse.rgb * ndotl;
    let local = lights_contrib(in.world_pos, nrm, material.light_diffuse.rgb, material.light_ambient.rgb);
    let lit = clamp(sun + local, vec3<f32>(0.0), vec3<f32>(1.0));
    var rgb = base.rgb * lit;
    let s = shadow_strength(in.world_pos, nrm, in.fog, dwx, dwy);
    rgb *= mix(1.0, frame.shadow.ctlb.y, s);
    // Blend toward the scene fog colour (matches GL33's mix(fogColor, r0, vFogTC)).
    rgb = mix(frame.fog_color.rgb, rgb, in.fog);
    return vec4<f32>(rgb, base.a);
}
