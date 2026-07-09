// GPU cull + LOD + indirect-arg compaction (docs/gpu-culling-and-depth-plan.md Stage 3).
//
// One thread per retained instance: frustum + distance + sub-pixel cull, pick a LOD, and
// atomic-append one DrawIndexedIndirect command per section of the chosen LOD into the
// per-pipeline-variant args buffer. This replaces the CPU per-object walk + LODShape draw
// (Scene::ObjectForDrawing / AdjustComplexity / Object::Draw) — the whole point being that
// with the CPU out of the per-object loop we can push draw distance + LOD detail well past
// what the CPU triangle budget allowed.
//
// LOD selection is seeded from the legacy FindSqrtLevel / LevelFromDistance2 SHAPE
// (dist² -> detail² -> resol² vs each level's resolution²), but EXACT parity is a non-goal:
// `lod_scale` is a tunable detail knob and the adaptive `_lodInvWidth` feedback loop is
// dropped for a fixed generous bias. Frustum + distance culling, by contrast, must be
// correct — dropping visible geometry or drawing everything are the real hazards.

struct CullParams {
    // World-space frustum planes (nx, ny, nz, d), normalized + oriented so a point is
    // INSIDE when dot(plane.xyz, p) + plane.w >= 0. Six planes; the near plane is the
    // reversed-Z-aware one (see frustum_planes_from_view_proj on the Rust side).
    frustum: array<vec4<f32>, 6>,
    cam_pos: vec4<f32>,      // world camera position (xyz)
    objects_z2: f32,         // draw distance² (distance cull)
    lod_scale: f32,          // Camera::Left() * detail_bias (LevelFromDistance2's `scale`, tunable)
    lod_inv_width: f32,      // detail multiplier (legacy _lodInvWidth; fixed, not fed back)
    pixel_limit: f32,        // sub-pixel cull threshold (legacy 0.125)
    instance_count: u32,
    variant_capacity: u32,   // max args per pipeline variant (partition stride into out_args)
    variant_count: u32,
    _pad: u32,
};

struct Instance {
    world: mat4x4<f32>,      // absolute model->world (read by the GPU-driven VS, not here)
    center: vec4<f32>,       // world bounding-sphere center (xyz), w = uniform scale
    model: u32,              // index into models[]
    flags: u32,
    _pad0: u32,
    _pad1: u32,
};

struct Model {
    lod_base: u32,           // first entry in lods[]
    lod_count: u32,          // drawable LOD levels (graphical only; special >=900 excluded)
    bounding_sphere: f32,    // model radius at scale 1
    _pad: u32,
};

struct Lod {
    resolution: f32,         // _resolutions[i]
    section_base: u32,       // first entry in sections[]
    section_count: u32,
    is_decal: u32,           // 1 = decal LOD (stepped past by the noDecal rule)
};

struct Section {
    first_index: u32,        // pool ibase + section start (into the shared index buffer)
    index_count: u32,
    base_vertex: u32,        // pool vbase
    variant: u32,            // pipeline-variant bucket (0..variant_count)
};

// Byte-identical to the renderer's DrawIndexedIndirectArgs (20 B) and the layout every
// backend's multi_draw_indexed_indirect consumes.
struct DrawArgs {
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    base_vertex: i32,
    first_instance: u32,
};

@group(0) @binding(0) var<uniform> cull: CullParams;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(0) @binding(2) var<storage, read> models: array<Model>;
@group(0) @binding(3) var<storage, read> lods: array<Lod>;
@group(0) @binding(4) var<storage, read> sections: array<Section>;
@group(0) @binding(5) var<storage, read_write> out_args: array<DrawArgs>;
// One atomic append cursor per pipeline variant.
@group(0) @binding(6) var<storage, read_write> counters: array<atomic<u32>>;
// Per-draw record parallel to out_args: which instance + which section this sub-draw is.
// A multi_draw sub-draw's first_instance indexes THIS (not the instance buffer directly),
// so the VS/FS can recover both the instance transform AND the per-section material —
// material is per-section, and the shader can't derive the section from the instance alone.
struct Record {
    instance: u32,
    section: u32,
};
@group(0) @binding(7) var<storage, read_write> out_records: array<Record>;

fn outside_frustum(center: vec3<f32>, radius: f32) -> bool {
    for (var i = 0u; i < 6u; i++) {
        let p = cull.frustum[i];
        if (dot(p.xyz, center) + p.w < -radius) {
            return true;
        }
    }
    return false;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= cull.instance_count) {
        return;
    }
    let inst = instances[idx];
    // Removed static slots are marked model = 0xFFFFFFFF (a free-list hole); this also
    // guards any out-of-range model id.
    if (inst.model >= arrayLength(&models)) {
        return;
    }
    let center = inst.center.xyz;
    let scale = inst.center.w;
    let model = models[inst.model];
    let radius = model.bounding_sphere * scale;

    // Distance (camera-relative) with the legacy near-clamp: a sphere large relative to its
    // distance is measured from its near surface so big near objects don't drop to a coarse
    // LOD (Scene.cpp:903).
    let rel = center - cull.cam_pos.xyz;
    var dist2 = dot(rel, rel);
    if (radius * radius > dist2 * 0.0625) {
        let dn = max(sqrt(dist2) - radius, 0.0);
        dist2 = dn * dn;
    }
    if (dist2 > cull.objects_z2) {
        return;
    }

    // Frustum: the planes are CAMERA-RELATIVE (extracted from proj * a translation-zeroed
    // view — the engine's geometry is camera-relative), so test the camera-relative center
    // `rel`, not the absolute `center`. Testing the absolute center here shifts the frustum
    // by cam_pos (kilometres on a real map) and culls erratically.
    if (outside_frustum(rel, radius)) {
        return;
    }

    // Sub-pixel: diameter² < (pixel_limit * lod_scale)² * detail²  -> too small to see.
    let detail2 = dist2 * cull.lod_inv_width * cull.lod_inv_width;
    let diameter = model.bounding_sphere * 2.0 * scale;
    let px = cull.pixel_limit * cull.lod_scale;
    if (diameter * diameter < px * px * detail2) {
        return;
    }

    // LOD select (FindSqrtLevel-style; ShapeLOD.cpp:1592). Walk resolutions ascending,
    // stop at the first level too rough for resol2, then step back past decal LODs.
    var level = 0u;
    if (model.lod_count >= 2u) {
        let resol2 = detail2 * cull.lod_scale * cull.lod_scale;
        var i = 1u;
        loop {
            if (i >= model.lod_count) { break; }
            let r = lods[model.lod_base + i].resolution;
            if (r * r > resol2) { break; }
            i++;
        }
        i -= 1u;
        loop {
            if (i == 0u || lods[model.lod_base + i].is_decal == 0u) { break; }
            i--;
        }
        level = i;
    }

    // Compaction: one DrawArgs per section of the chosen LOD, appended to its pipeline
    // variant's partition. first_instance = the retained instance index, so the VS reads
    // this instance's world/material straight from the retained buffers (no gather).
    // instance_count = 1 (per-section instancing collapse is a later optimization).
    let lod = lods[model.lod_base + level];
    for (var s = 0u; s < lod.section_count; s++) {
        let sec = sections[lod.section_base + s];
        let v = sec.variant;
        if (v >= cull.variant_count) {
            continue;
        }
        let slot = atomicAdd(&counters[v], 1u);
        // Overflow past the per-variant cap is dropped, never wrapped; the CPU reads the
        // counter and logs if a frame exceeded the cap (never a silent partial draw).
        if (slot >= cull.variant_capacity) {
            continue;
        }
        let out_i = v * cull.variant_capacity + slot;
        // first_instance = the record slot; the VS reads out_records[record] to get the
        // instance transform + the global section id for its material.
        out_args[out_i] = DrawArgs(sec.index_count, 1u, sec.first_index, i32(sec.base_vertex), out_i);
        out_records[out_i] = Record(idx, lod.section_base + s);
    }
}
