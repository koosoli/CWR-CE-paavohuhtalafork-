// Unlit textured 3D, plain (vs_main) and GPU-skinned (vs_skinned) variants
// sharing fs_main. The two group(1) declarations coexist because each entry
// point statically uses only one (binding collisions are validated per entry
// point): the plain pipeline binds `object`, the skinned pipeline binds the
// bone `palette` (from the skin module). Shadowing is the shared cascade kernel.

#import frame::{frame, reverse_z, fog_factor, terrain_sun_shadow}
#import shadow::shadow_strength
#import skin::{skin_pos, skin_normal}
#import lighting::lights_contrib
// Group(4) terrain heightmap + surface_y, shared with the shadow depth pass.
#import conform::surface_y

struct Object {
    world: mat4x4<f32>,
    // Terrain-conform plane, published per instance by the CPU (ForestPlain).
    // When conform2.z (mode) > 0 the vertex shader conforms this instance to the ground
    // exactly like ForestPlain::Animate's two-triangle bilinear fit, so the shared
    // forest mesh can be uploaded ONCE undeformed instead of rewritten per instance.
    conform0: vec4<f32>,   // inv_land_grid, -xf, -zf, bias(=BoundingCenter().y)
    conform1: vec4<f32>,   // y00, y10, d1000, d0100
    conform2: vec4<f32>,   // d1011, d0111, mode(0=none,1=forest), _pad
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
    // Sun-only Blinn-Phong highlight (GL33's c18): rgb = sun diffuse x material
    // specular (sun-enable folded in), w = power. Added per-fragment when w > 0.
    specular: vec4<f32>,
};

// Per-draw world matrix + material as read-only storage arrays, indexed by
// @builtin(instance_index) (fed as the draw's base_instance). One upload per
// frame, group(1) bound once — no per-draw dynamic offsets. The skinned pipeline
// binds the bone `palette` (from the skin module) at binding(0) instead of
// `objects`; each entry point statically uses only one, so both coexist.
@group(1) @binding(0) var<storage, read> objects: array<Object>;   // plain pipeline
@group(1) @binding(1) var<storage, read> materials: array<Material>;
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

// Baked per-pipeline (pipeline-overridable constants): the alpha-test cutout
// threshold and whether this is a shadow-darken pipeline. Keeping them out of a
// per-draw binding avoids a 5th bind group (wgpu's default maxBindGroups is 4).
override alpha_ref: f32 = 0.0;   // discard fragments with alpha below this (0 = off)
override is_shadow: f32 = 0.0;   // 1 = output black + shadow-strength alpha
// Brightness a fully terrain-shadowed alpha-tested surface (foliage cutout) keeps.
// Dense canopy self-occludes its sky ambient — which the world-space terrain mask
// can't model and foliage materials inflate for the sunlit look — so shadowed
// leaves stay too bright under the ambient-preserving model that suits solid
// ground/decals. This extra multiply darkens only alpha-tested foliage in terrain
// shadow toward the close-up CSM look. 1 = off (no extra darkening).
override foliage_shadow_ao: f32 = 0.35;
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
    // Draw slot, carried flat so fs_main can index its material. Equals the draw's
    // base_instance (see draw_one); both vertex paths pass it through.
    @location(4) @interpolate(flat) instance: u32,
};

// world_pos is camera-relative (the world matrix / palette is offset by the
// camera position on the C++ side), so its length is the camera distance.
fn finish_vertex(world_pos: vec4<f32>, normal_ws: vec3<f32>, uv: vec2<f32>, instance: u32) -> VsOut {
    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * world_pos);
    // Bias toward the camera (larger reversed depth) so decals/overlays win the
    // depth test against coplanar geometry.
    out.clip.z += depth_bias * out.clip.w;
    out.uv = uv;
    out.world_pos = world_pos.xyz;
    out.normal = normal_ws;
    out.fog = fog_factor(length(world_pos.xyz));
    out.instance = instance;
    return out;
}

@vertex
fn vs_main(
    @builtin(instance_index) instance: u32,
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(5) conform_sel: u32,   // per-vertex conform selector (mode 2): 0/1/2
) -> VsOut {
    let obj = objects[instance];
    let world = obj.world;
    var world_pos = world * vec4<f32>(pos, 1.0);
    // Vertex normals arrive already negated (MeshBuild::BuildVertices stores
    // -Norm, the D3D convention). GL33 rotates that stored normal to world space
    // and lights with it as-is (EngineGL33_Shaders VSNormal: `mat3(world)*normal`,
    // no extra negation), so do the same here — negating again would flip N and
    // invert the diffuse/specular/local-light terms relative to the sun.
    let rot = mat3x3<f32>(world[0].xyz, world[1].xyz, world[2].xyz);
    var normal_ws = rot * norm;
    // Terrain conform: the shared base mesh is uploaded undeformed and conformed here
    // per instance. world_pos is camera-relative, so heights are evaluated in ABSOLUTE
    // world xz (+ frame.cam_pos) and written back camera-relative. conform2.z = mode:
    // 1 = ForestPlain bilinear plane, 2 = per-vertex ClipLand vegetation (heightmap).
    let mode = obj.conform2.z;
    if (mode > 1.5) {
        // Mode 2: individual ClipLand vegetation, conformed per vertex to SurfaceY,
        // matching Object::Animate (Object.cpp:395-423). conform_sel: 1 = ClipLandKeep
        // (keep height above the surface), 2 = ClipLandOn (pin onto it), 0 = rigid.
        let abs_x = world_pos.x + frame.cam_pos.x;
        let abs_z = world_pos.z + frame.cam_pos.z;
        let sy = surface_y(vec2<f32>(abs_x, abs_z));
        if (conform_sel == 1u) {
            // world.y_abs = SurfaceY + undeformedWorldY - bcSurfaceY (conform0.x); the
            // cam.y offset cancels between the two camera-relative terms.
            world_pos.y = sy + world_pos.y - obj.conform0.x;
        } else if (conform_sel == 2u) {
            world_pos.y = sy - frame.cam_pos.y;
        }
        // Normals: keep the model normal for now (foliage is lit near-flat); the CPU
        // recomputes them from the conformed faces, a refinement to revisit if needed.
    } else if (mode > 0.5) {
        // Mode 1: ForestPlain bilinear plane fit (ObjectClasses.cpp:571-605).
        let s = obj.conform0.x;                          // inv_land_grid
        let xIn = (world_pos.x + frame.cam_pos.x) * s + obj.conform0.y;  // *invLand - xf
        let zIn = (world_pos.z + frame.cam_pos.z) * s + obj.conform0.z;  // *invLand - zf
        let y00 = obj.conform1.x; let y10 = obj.conform1.y;
        let d1000 = obj.conform1.z; let d0100 = obj.conform1.w;
        let d1011 = obj.conform2.x; let d0111 = obj.conform2.y;
        let triA = xIn <= 1.0 - zIn;
        let py = select(y10 + d0111 - d1011 * xIn - zIn * d0111,
                        y00 + d1000 * zIn + d0100 * xIn,
                        triA);
        // Camera-relative conformed height: absolute plane height + the vertex's own
        // model height above surface (conform0.w = BoundingCenter().y), minus cam.y.
        world_pos.y = py - frame.cam_pos.y + pos.y + obj.conform0.w;
        // Tilt the undeformed normal by the plane gradient (inverse-transpose of the
        // affine y-shear) so lighting matches the CPU's post-deform InvalidateNormals.
        let gx = select(-d1011, d0100, triA) * s;
        let gz = select(-d0111, d1000, triA) * s;
        normal_ws = vec3<f32>(normal_ws.x - gx * normal_ws.y, normal_ws.y, normal_ws.z - gz * normal_ws.y);
    }
    return finish_vertex(world_pos, normal_ws, uv, instance);
}

// Linear-blend skinning (see the skin module). Vertices with no skin weight
// carry a single weight of 1.0 on a reserved bone whose palette entry is just
// `world`, so no zero-weight fallback is needed.
@vertex
fn vs_skinned(
    @builtin(instance_index) instance: u32,
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bones: vec4<u32>,   // Uint8x4: palette indices
    @location(4) weights: vec4<f32>, // Unorm8x4: normalised weights
) -> VsOut {
    let world_pos = skin_pos(pos, bones, weights);
    // As in vs_main: `norm` is the already-negated stored normal (SetSkinData
    // uploads -OrigNorm), skin_normal rotates it into world space, and we light
    // with it as-is to match GL33 — no extra negation.
    let normal_ws = skin_normal(norm, bones, weights);
    return finish_vertex(world_pos, normal_ws, uv, instance);
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
    // Per-draw material for this draw slot (base_instance == draw slot).
    let material = materials[in.instance];
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
    // Long-range terrain sun-shadow (a mountain casting onto this object): removes
    // the direct sun — diffuse + specular — but keeps ambient/emissive/local, the
    // same model the terrain uses, so an object darkens like the ground it stands on
    // and never goes black. Sampled by the object's absolute world position
    // (world_pos is camera-relative). CSM (below) still handles near object shadows.
    let world_abs = in.world_pos + frame.cam_pos.xyz;
    let terrain_s = terrain_sun_shadow(world_abs.xz, world_abs.y);
    let sun_vis = 1.0 - terrain_s;
    let sun = material.emissive.rgb + material.sun_ambient.rgb
              + material.sun_diffuse.rgb * ndotl * sun_vis;
    let local = lights_contrib(in.world_pos, nrm, material.light_diffuse.rgb, material.light_ambient.rgb);
    let lit = clamp(sun + local, vec3<f32>(0.0), vec3<f32>(1.0));
    var rgb = base.rgb * lit;
    // Sun-only Blinn-Phong specular (GL33's PSSpecular, moved per-fragment):
    // untextured, additive, added before the shadow multiply so a shadowed
    // surface loses its highlight. world_pos is camera-relative (camera at the
    // origin), so the view direction is normalize(-world_pos), NOT via cam_pos.
    // sun_dir_world is the light travel direction — negated like the N.L term.
    if (material.specular.w > 0.0) {
        let view_dir = normalize(-in.world_pos);
        let half_vec = normalize(-frame.sun_dir_world.xyz + view_dir);
        let n_dot_h = max(dot(nrm, half_vec), 0.0);
        let spec = material.specular.rgb * pow(n_dot_h, max(material.specular.w, 1.0));
        rgb += clamp(spec * sun_vis, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    // Canopy self-occlusion for alpha-tested foliage in terrain shadow (see the
    // foliage_shadow_ao note). Terrain-shadow only — CSM already darkens near
    // foliage correctly — and skipped for solids so ground decals keep matching the
    // terrain. Compile-time branch (alpha_ref is a pipeline constant), no divergence.
    if (alpha_ref > 0.0) {
        rgb *= mix(1.0, foliage_shadow_ao, terrain_s);
    }
    let s = shadow_strength(in.world_pos, nrm, in.fog, dwx, dwy);
    rgb *= mix(1.0, frame.shadow.ctlb.y, s);
    // Blend toward the scene fog colour (matches GL33's mix(fogColor, r0, vFogTC)).
    rgb = mix(frame.fog_color.rgb, rgb, in.fog);
    return vec4<f32>(rgb, base.a);
}
