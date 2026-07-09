// GPU-driven object draw (docs/gpu-culling-and-depth-plan.md Stage 3). Consumes the cull
// compute's output: multi_draw_indexed_indirect over out_args, one sub-draw per surviving
// (section, instance) pair, first_instance = the record slot. The VS reads the record to
// find the instance transform (absolute world -> camera-relative in-shader) and the global
// section id; the FS folds that section's RAW material with the frame sun (the fold GL33 /
// the per-draw path do CPU-side) and hands it to the shared shade() so the lit look matches
// the per-draw path exactly.
//
// Rigid opaque only: terrain-conformed vegetation, skinned, and transparent draws stay on
// the CPU path (they never enter the retained instance set), so this VS has no conform.

#import frame::{frame, reverse_z, fog_factor}
#import shading::{shade, ShadeMaterial}
#import gbuffer::oct_encode
// Terrain conform (group 4 heightmap + surface_y/surface_grad), shared with shader3d /
// shadow_depth. Lets the GPU-driven VS conform ClipLand vegetation/fences to the ground per
// vertex, matching the per-draw path — so these objects no longer have to stay on the CPU.
#import conform::{surface_y, surface_grad}

// Per-instance conform: flags bit 0 set = this instance is terrain-conformed (ClipLand,
// mode 2); instance._pad0 then carries bcSurfaceY (bitcast f32) — the surface height at the
// object's ground reference, matching Object::PublishConformPlane.
const CONFORM_CLIPLAND: u32 = 1u;

// Pipeline-overridable constants (kept out of a per-draw binding), same as shader3d.
override linear: f32 = 0.0;
override foliage_shadow_ao: f32 = 0.35;

// Mirrors InstanceGpu / RecordGpu / SectionMaterialGpu (cull.rs).
struct Instance {
    world: mat4x4<f32>,
    center: vec4<f32>,
    model: u32,
    flags: u32,
    _pad0: u32,
    _pad1: u32,
};
struct Record {
    instance: u32,
    section: u32,
};
struct SectionMaterial {
    emissive: vec4<f32>,
    ambient: vec4<f32>,
    diffuse: vec4<f32>,
    specular: vec4<f32>, // w = specular power
    texture_slot: u32,
    sampler_idx: u32,
    alpha_ref: f32,
    _pad: u32,
};

@group(1) @binding(0) var<storage, read> instances: array<Instance>;
@group(1) @binding(1) var<storage, read> records: array<Record>;
@group(1) @binding(2) var<storage, read> section_materials: array<SectionMaterial>;
@group(2) @binding(0) var textures: binding_array<texture_2d<f32>>;
@group(3) @binding(0) var samplers: binding_array<sampler, 8>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fog: f32,
    @location(2) world_pos: vec3<f32>, // camera-relative
    @location(3) normal: vec3<f32>,    // world space, outward
    @location(4) @interpolate(flat) section: u32,
};

@vertex
fn vs_gpu(
    @builtin(instance_index) rec_slot: u32,
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(5) conform_sel: u32, // per-vertex conform selector (mode 2): 0 rigid / 1 keep / 2 on
) -> VsOut {
    let rec = records[rec_slot];
    let inst = instances[rec.instance];
    let world = inst.world;
    // Absolute -> camera-relative (the per-draw path pre-offsets on the CPU; here the
    // transform is absolute, so subtract cam_pos). view has its translation zeroed.
    let world_pos_abs = world * vec4<f32>(pos, 1.0);
    var world_pos = world_pos_abs.xyz - frame.cam_pos.xyz;
    // Normals arrive already negated (MeshBuild stores -Norm); rotate to world and light
    // as-is, matching vs_main / GL33.
    let rot = mat3x3<f32>(world[0].xyz, world[1].xyz, world[2].xyz);
    var normal_ws = rot * norm;
    // Terrain conform (mode 2 / individual ClipLand veg + fences): conform each vertex to
    // SurfaceY, matching shader3d::vs_main (Object::Animate). Absolute world xz is
    // world_pos_abs.xz; heights are written back camera-relative. conform_sel: 1 = ClipLandKeep
    // (keep the vertex's height above the surface), 2 = ClipLandOn (pin onto it), 0 = rigid.
    if ((inst.flags & CONFORM_CLIPLAND) != 0u) {
        let sy = surface_y(world_pos_abs.xz);
        if (conform_sel == 1u) {
            // world.y_abs = SurfaceY + undeformedWorldY - bcSurfaceY; the cam.y offset cancels
            // between the two camera-relative terms.
            world_pos.y = sy + world_pos.y - bitcast<f32>(inst._pad0);
        } else if (conform_sel == 2u) {
            world_pos.y = sy - frame.cam_pos.y;
        }
        // Tilt the conformed vertex's normal by the terrain slope so lighting follows the
        // ground (same shear as shader3d). Rigid verts (sel 0) keep theirs.
        if (conform_sel != 0u) {
            let g = surface_grad(world_pos_abs.xz);
            normal_ws = vec3<f32>(normal_ws.x - g.x * normal_ws.y, normal_ws.y,
                                  normal_ws.z - g.y * normal_ws.y);
        }
    }
    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * vec4<f32>(world_pos, 1.0));
    out.uv = uv;
    out.world_pos = world_pos;
    out.normal = normal_ws;
    out.fog = fog_factor(length(world_pos));
    out.section = rec.section;
    return out;
}

@fragment
fn fs_gpu(in: VsOut) -> @location(0) vec4<f32> {
    let dwx = dpdx(in.world_pos);
    let dwy = dpdy(in.world_pos);
    let sm = section_materials[in.section];
    // The section id is uniform across a derivative quad (one section per primitive), so the
    // bindless index stays uniform and implicit-mip sampling is legal.
    let base = textureSample(textures[sm.texture_slot], samplers[sm.sampler_idx], in.uv);
    if (base.a < sm.alpha_ref) {
        discard;
    }
    // Fold RAW material x the frame sun, reproducing GL33's UploadVSMaterialConstants
    // (EngineWgpu.cpp) in-shader: sun_* = sun x material (legacy path only), light_* = raw
    // material (local lights), specular = sun_diffuse x material specular. On the sky-lit HDR
    // path shade() ignores sun_*, so the emissive/light/specular terms are what carry.
    var m: ShadeMaterial;
    m.emissive = sm.emissive.rgb;
    m.sun_ambient = frame.sun_ambient.rgb * sm.ambient.rgb;
    m.sun_diffuse = frame.sun_diffuse.rgb * sm.diffuse.rgb;
    m.light_diffuse = sm.diffuse.rgb;
    m.light_ambient = sm.ambient.rgb;
    m.specular = frame.sun_diffuse.rgb * sm.specular.rgb;
    m.spec_power = sm.specular.w;
    let rgb = shade(
        base.rgb, m, in.normal, in.world_pos, in.fog, dwx, dwy, linear, foliage_shadow_ao,
        sm.alpha_ref > 0.0,
    );
    return vec4<f32>(rgb, base.a);
}

// Depth+normal PREPASS fragment: no shading — writes ONLY the view-space octahedral normal
// into the Rg16Float G-buffer (depth is written by the fixed-function stage). Mirrors
// shader3d's fs_prepass, but reads the per-section cutout threshold + bindless texture from
// section_materials (the GPU-driven path is per-section, not per-instance). Cutout foliage
// applies the SAME discard as fs_gpu so prepass coverage matches the colour pass.
@fragment
fn fs_gpu_prepass(in: VsOut) -> @location(0) vec2<f32> {
    let sm = section_materials[in.section];
    if (sm.alpha_ref > 0.0) {
        if (textureSample(textures[sm.texture_slot], samplers[sm.sampler_idx], in.uv).a < sm.alpha_ref) {
            discard;
        }
    }
    // in.normal is world space; the view matrix's translation is zeroed, so transforming the
    // direction gives view space (matches shader3d::fs_prepass).
    let n_view = (frame.view * vec4<f32>(normalize(in.normal), 0.0)).xyz;
    return oct_encode(normalize(n_view));
}
