// Debug visualization of the GPU-driven frustum-cull spheres (WGR_GPU_DRIVEN). One instanced
// LINE-LIST draw per retained instance renders a 3-ring wireframe sphere at the EXACT centre +
// radius the cull compute tests with (center = instance.center.xyz, radius =
// models[instance.model].bounding_sphere * instance.center.w). Lets a "disappears when close /
// floats" object be checked by eye: does the sphere sit on the object, and where is its centre
// vs the ground? Toggled from the ImGui Culling tab; draws nothing when off.
//
// This reads the SAME retained buffers the cull consumes, so it is ground-truth for what the
// cull sees — not a CPU re-derivation that could drift.

#import frame::{frame, reverse_z}

// Must match InstanceGpu (cull.rs) / the cull.wgsl Instance stride (144 B). Only center + model
// are read here; the rest is present for stride.
struct Instance {
    world: mat4x4<f32>,
    center: vec4<f32>,   // xyz = world bounding-sphere centre, w = uniform scale
    model: u32,
    flags: u32,
    pad0: u32,
    pad1: u32,
    conform0: vec4<f32>,
    conform1: vec4<f32>,
    conform2: vec4<f32>,
};

struct Model {
    lod_base: u32,
    lod_count: u32,
    bounding_sphere: f32, // radius at scale 1
    pad: u32,
};

@group(1) @binding(0) var<storage, read> instances: array<Instance>;
@group(1) @binding(1) var<storage, read> models: array<Model>;

// Segments per ring. 3 rings * SEG segments * 2 endpoints = 6*SEG vertices per instance.
const SEG: u32 = 32u;
const TAU: f32 = 6.2831853;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_sphere(@builtin(vertex_index) vidx: u32, @builtin(instance_index) iidx: u32) -> VsOut {
    var out: VsOut;
    let inst = instances[iidx];
    // Removed static slots are model = 0xFFFFFFFF (free-list hole); collapse them off-screen.
    if (inst.model >= arrayLength(&models)) {
        out.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        out.color = vec3<f32>(0.0);
        return out;
    }
    let center = inst.center.xyz;
    let scale = inst.center.w;
    // Show the radius the FRUSTUM test actually uses: the model sphere, or the inflated
    // per-instance radius (pad0, f32 bits) a terrain-conform instance carries.
    let model_radius = models[inst.model].bounding_sphere * scale;
    let radius = select(model_radius, bitcast<f32>(inst.pad0), inst.pad0 != 0u);

    let per_ring = SEG * 2u;
    let ring = vidx / per_ring;
    let local = vidx % per_ring;
    let seg = local / 2u;
    let end = local % 2u;            // line-list: each segment is [seg, seg+1]
    let ang = TAU * f32(seg + end) / f32(SEG);
    let c = cos(ang);
    let s = sin(ang);
    var unit = vec3<f32>(0.0, 0.0, 0.0);
    if (ring == 0u) {
        unit = vec3<f32>(c, 0.0, s);   // horizontal equator (XZ)
    } else if (ring == 1u) {
        unit = vec3<f32>(c, s, 0.0);   // vertical, faces +Z (XY)
    } else {
        unit = vec3<f32>(0.0, c, s);   // vertical, faces +X (YZ)
    }

    // Camera-relative (the engine's geometry convention), exactly like the GPU-driven VS.
    let world_pos = center - frame.cam_pos.xyz + unit * radius;
    out.clip = reverse_z(frame.proj * frame.view * vec4<f32>(world_pos, 1.0));
    // Colour by conform mode (values scaled up to survive HDR tonemapping):
    //   rigid (0)          -> green
    //   ForestPlain (1)    -> blue
    //   ClipLand veg (2)   -> orange
    let mode = inst.conform2.z;
    if (mode > 1.5) {
        out.color = vec3<f32>(3.0, 1.2, 0.1);
    } else if (mode > 0.5) {
        out.color = vec3<f32>(0.3, 0.9, 3.0);
    } else {
        out.color = vec3<f32>(0.3, 3.0, 0.6);
    }
    return out;
}

@fragment
fn fs_sphere(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
