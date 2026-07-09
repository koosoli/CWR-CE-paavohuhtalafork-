// GPU cull + LOD + indirect-arg compaction (docs/gpu-culling-and-depth-plan.md Stage 3).
// See cull.wgsl for the compute; this owns the GPU-side data layouts (filled from C++ via
// the retained-scene FFI in Stage 3b), the compute pipeline, and the CPU-side frustum-plane
// extraction that feeds the cull params each frame.
//
// Stage 3a scope: the data model + shader + pipeline + the (testable) frustum math, with no
// live data source yet. Stage 3b allocates/uploads the buffers from the C++ world walk,
// dispatches the compute, and submits its args via multi_draw_indexed_indirect.

use bytemuck::Zeroable;
use glam::{Mat4, Vec3, Vec4};

// --- GPU buffer layouts (mirror the structs in cull.wgsl exactly) ---

// Per-frame cull parameters (one uniform). 144 bytes, 16-aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CullParamsGpu {
    // World-space planes (nx, ny, nz, d), oriented so dot(n, p) + d >= 0 == inside.
    pub frustum: [[f32; 4]; 6],
    pub cam_pos: [f32; 4],
    pub objects_z2: f32,
    pub lod_scale: f32,
    pub lod_inv_width: f32,
    pub pixel_limit: f32,
    pub instance_count: u32,
    pub variant_capacity: u32,
    pub variant_count: u32,
    pub _pad: u32,
}

// One retained instance. `world` is the ABSOLUTE model->world transform (the GPU-driven VS
// subtracts cam_pos in-shader); `center.xyz` is the transformed bounding-sphere center and
// `center.w` the uniform scale — both used by the cull compute (which never reads `world`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceGpu {
    pub world: [f32; 16],
    pub center: [f32; 4],
    pub model: u32,
    // Bit 0 (CONFORM_CLIPLAND) = terrain-conformed instance; the GPU-driven VS then conforms it
    // to SurfaceY per vertex (mode 2). Other bits reserved.
    pub flags: u32,
    // For a conform instance, bcSurfaceY (bitcast f32) — the surface height at the object's
    // ground reference (Object::PublishConformPlane). Unused (0) otherwise.
    pub _pad0: u32,
    pub _pad1: u32,
}

// One model = a range of drawable LOD levels + its bounding radius at scale 1.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ModelGpu {
    pub lod_base: u32,
    pub lod_count: u32,
    pub bounding_sphere: f32,
    pub _pad: u32,
}

// One LOD level = a resolution threshold + a range of sections.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LodGpu {
    pub resolution: f32,
    pub section_base: u32,
    pub section_count: u32,
    pub is_decal: u32,
}

// One drawable section = a slice of the shared geometry pool + its pipeline variant.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SectionGpu {
    pub first_index: u32,
    pub index_count: u32,
    pub base_vertex: u32,
    pub variant: u32,
}

// One per-draw record, written by the compute parallel to out_args: the instance + the
// global section this sub-draw renders. A sub-draw's first_instance indexes this, so the
// VS/FS recovers both the instance transform and the per-section material.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RecordGpu {
    pub instance: u32,
    pub section: u32,
}

// Per-section shading (static, register-once): raw material the GPU-driven FS folds with
// the frame sun, plus the bindless texture slot / sampler / cutout threshold. Indexed by
// the global section id in RecordGpu. Parallel to the section table.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SectionMaterialGpu {
    pub emissive: [f32; 4],
    pub ambient: [f32; 4],
    pub diffuse: [f32; 4],
    pub specular: [f32; 4], // w = specular power
    pub texture_slot: u32,
    pub sampler: u32,
    pub alpha_ref: f32,
    pub _pad: u32,
}

// A permissive plane that never culls anything: dot(0, p) + BIG >= -radius always holds.
// Used for the far plane (radial distance is culled by objects_z2 instead of a flat far
// plane, which would clip peripheral content early — see the frustum-cull hazard note).
const NO_OP_PLANE: [f32; 4] = [0.0, 0.0, 0.0, 1.0e30];

/// Extract 6 **camera-relative** cull planes for the compute, oriented so `dot(n, p) + d >= 0`
/// means inside — where `p` is a CAMERA-RELATIVE position (`world - cam_pos`). `view_proj` must
/// be `proj * view` with the view's translation ZEROED (the engine's convention: geometry is
/// camera-relative), so Gribb–Hartmann yields planes in camera-relative space and the compute
/// tests instance centers as `center - cam_pos`.
///
/// Every plane is derived from `proj * view` itself (Gribb–Hartmann), so ALL are exactly
/// consistent with the actual projection for ANY orientation:
///   left/right/top/bottom = row3 ± row0/row1 — the classic side planes;
///   near = row3 = the `clip.w >= 0` half-space. `clip.w` is precisely "in front of the camera"
///     for the true projection (only points with `clip.w > 0` render), so this is the correct,
///     handedness-agnostic near plane — NOT a hand-picked forward axis (the engine's row-major
///     D3D LH layout makes a naive `view.z_axis`/`-Z` guess point the wrong way, which culls a
///     direction-dependent half-space that pops geometry in/out as the camera rotates). The
///     camera-relative view puts `clip.w = z_view = 0` at the origin, so near passes through the
///     camera (a through-apex plane: in/out by direction, not distance — exactly a view cone).
/// The far slot is a no-op: radial distance culling (`objects_z2`) replaces a flat far plane.
pub fn frustum_planes(view_proj: Mat4) -> [[f32; 4]; 6] {
    let r0 = view_proj.row(0);
    let r1 = view_proj.row(1);
    let r3 = view_proj.row(3);

    // Normalize by the length of the plane's xyz so the `-radius` slack in the shader is in
    // world units.
    let norm = |p: Vec4| -> [f32; 4] {
        let n = Vec3::new(p.x, p.y, p.z);
        let len = n.length();
        let inv = if len > 0.0 { 1.0 / len } else { 0.0 };
        [p.x * inv, p.y * inv, p.z * inv, p.w * inv]
    };

    let left = norm(r3 + r0);
    let right = norm(r3 - r0);
    let bottom = norm(r3 + r1);
    let top = norm(r3 - r1);
    let near = norm(r3);

    [left, right, bottom, top, near, NO_OP_PLANE]
}

// --- Compute pipeline ---

// Pipeline-variant buckets: which opaque variants the cull's compaction groups sections
// into (one indirect batch + one multi_draw per variant). The opaque set is small; solid
// vs alpha-cutout is the split that matters. Grown as needed.
pub const CULL_VARIANT_COUNT: u32 = 2;

// Marks a removed static instance slot (a free-list hole); the compute skips it.
pub const INVALID_MODEL: u32 = u32::MAX;

// Default per-variant arg capacity. out_args holds CULL_VARIANT_COUNT * this DrawArgs;
// overflow past it is dropped and logged (never a silent partial draw), and it can grow.
const DEFAULT_VARIANT_CAPACITY: u32 = 1 << 16; // 64K sections/variant

// u32 words per DrawIndexedIndirectArgs (20 B / 4).
const ARG_WORDS: u64 = super::INDIRECT_ARG_SIZE / 4;

pub struct CullState {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,

    // Retained tables — CPU mirrors + GPU buffers. Registered at load, rarely changed, so
    // re-uploaded wholesale when `tables_dirty`.
    models: Vec<ModelGpu>,
    lods: Vec<LodGpu>,
    sections: Vec<SectionGpu>,
    // Per-section shading, parallel to `sections` (same global index). Draw-side only —
    // not a compute input; bound in the GPU-driven draw pass (3b-2b).
    section_materials: Vec<SectionMaterialGpu>,
    tables_dirty: bool,

    // Unified instance buffer: static slots [0, static.len()) with a free-list, then the
    // dynamic region re-copied every frame. A removed static slot has model = INVALID_MODEL.
    static_instances: Vec<InstanceGpu>,
    free_slots: Vec<u32>,
    static_dirty: Option<(u32, u32)>, // inclusive min..=max changed static slot this frame
    dynamic: Vec<InstanceGpu>,
    // Static region length uploaded last frame, so a grown static region re-copies the
    // dynamic tail to its new offset.
    uploaded_static_len: u32,

    model_buf: super::StorageArray,
    lod_buf: super::StorageArray,
    section_buf: super::StorageArray,
    section_mat_buf: super::StorageArray,
    instance_buf: super::StorageArray,
    // Per-variant append cursors written by the compute. Fixed size (CULL_VARIANT_COUNT
    // words) and carries INDIRECT usage so it can double as the count buffer for
    // multi_draw_indexed_indirect_count (the 3b-4 tail trim) on adapters that support it.
    counter_buf: wgpu::Buffer,
    out_args: Option<wgpu::Buffer>,
    // Per-draw records, allocated 1:1 with out_args (same slot count, 8 B each).
    out_records: Option<wgpu::Buffer>,
    out_args_cap: u64,
    params_buf: wgpu::Buffer,

    variant_capacity: u32,
    params: CullParamsGpu,
    bind: Option<wgpu::BindGroup>,
}

impl CullState {
    // Byte size of one CullParamsGpu (the uniform is fixed-size).
    const PARAMS_SIZE: u64 = std::mem::size_of::<CullParamsGpu>() as u64;

    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_cull"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cull.wgsl").into()),
        });

        // Bindings 0..=6, matching cull.wgsl. 0 uniform, 1..=4 read-only storage,
        // 5..=6 read-write storage (the args + per-variant counters the compute writes).
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_cull_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, true),
                storage(5, false),
                storage(6, false),
                storage(7, false),
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_cull_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("wgr_cull_pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_cull_params"),
            size: Self::PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let counter_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_cull_counters"),
            size: CULL_VARIANT_COUNT as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            layout,
            models: Vec::new(),
            lods: Vec::new(),
            sections: Vec::new(),
            section_materials: Vec::new(),
            tables_dirty: false,
            static_instances: Vec::new(),
            free_slots: Vec::new(),
            static_dirty: None,
            dynamic: Vec::new(),
            uploaded_static_len: 0,
            model_buf: super::StorageArray::new("wgr_cull_models"),
            lod_buf: super::StorageArray::new("wgr_cull_lods"),
            section_buf: super::StorageArray::new("wgr_cull_sections"),
            section_mat_buf: super::StorageArray::new("wgr_cull_section_mats"),
            instance_buf: super::StorageArray::new("wgr_cull_instances"),
            counter_buf,
            out_args: None,
            out_records: None,
            out_args_cap: 0,
            params_buf,
            variant_capacity: DEFAULT_VARIANT_CAPACITY,
            params: CullParamsGpu::zeroed(),
            bind: None,
        }
    }

    // Register a model: its sections (already resolved to pool offsets by the caller) and
    // its drawable LOD levels (whose `section_base` is RELATIVE to `sections`). Appends to
    // the global tables and returns the model id (index into models[]). Called at load.
    pub fn register_model(
        &mut self,
        bounding_sphere: f32,
        lods: &[LodGpu],
        sections: &[SectionGpu],
        materials: &[SectionMaterialGpu],
    ) -> u32 {
        debug_assert_eq!(sections.len(), materials.len(), "one material per section");
        let section_offset = self.sections.len() as u32;
        let lod_offset = self.lods.len() as u32;
        self.sections.extend_from_slice(sections);
        self.section_materials.extend_from_slice(materials);
        for lod in lods {
            let mut l = *lod;
            l.section_base += section_offset;
            self.lods.push(l);
        }
        let model_id = self.models.len() as u32;
        self.models.push(ModelGpu {
            lod_base: lod_offset,
            lod_count: lods.len() as u32,
            bounding_sphere,
            _pad: 0,
        });
        self.tables_dirty = true;
        model_id
    }

    // Replace the whole sections table with a freshly-resolved one (same length + order as
    // registration, only base_vertex/first_index may have moved). Marks the tables dirty for
    // re-upload only when something actually changed, so a quiet frame costs nothing. The LOD
    // section_base offsets index this table and are unaffected (order is preserved).
    pub fn set_sections(&mut self, sections: &[SectionGpu]) {
        if self.sections.as_slice() != sections {
            self.sections.clear();
            self.sections.extend_from_slice(sections);
            self.tables_dirty = true;
        }
    }

    // Add a static instance; returns its stable slot (reused from the free-list if any).
    pub fn instance_add(&mut self, inst: InstanceGpu) -> u32 {
        let slot = if let Some(s) = self.free_slots.pop() {
            self.static_instances[s as usize] = inst;
            s
        } else {
            self.static_instances.push(inst);
            (self.static_instances.len() - 1) as u32
        };
        self.mark_static_dirty(slot);
        slot
    }

    pub fn instance_update(&mut self, slot: u32, inst: InstanceGpu) {
        if let Some(e) = self.static_instances.get_mut(slot as usize) {
            *e = inst;
            self.mark_static_dirty(slot);
        }
    }

    // Remove a static instance: mark its slot a free-list hole (model = INVALID_MODEL, so
    // the compute skips it) and recycle the slot.
    pub fn instance_remove(&mut self, slot: u32) {
        if let Some(e) = self.static_instances.get_mut(slot as usize) {
            e.model = INVALID_MODEL;
            self.free_slots.push(slot);
            self.mark_static_dirty(slot);
        }
    }

    // Replace the whole dynamic set (re-copied every frame — the churny set the CPU already
    // walks for simulation).
    pub fn set_dynamic(&mut self, instances: &[InstanceGpu]) {
        self.dynamic.clear();
        self.dynamic.extend_from_slice(instances);
    }

    // Set this frame's cull params (frustum/cam/objectsZ/lod knobs). instance_count and the
    // variant fields are filled by prepare().
    pub fn set_params(&mut self, params: CullParamsGpu) {
        self.params = params;
    }

    fn mark_static_dirty(&mut self, slot: u32) {
        self.static_dirty = Some(match self.static_dirty {
            Some((lo, hi)) => (lo.min(slot), hi.max(slot)),
            None => (slot, slot),
        });
    }

    // Upload dirty tables + instances + params and (re)build the bind group. Call once per
    // frame before dispatch. Returns whether any GPU buffer was (re)allocated, so the
    // GPU-driven draw's group-1 bind group (which borrows instances/records/materials) is
    // rebuilt only when one actually moved.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let mut grew = false;

        if self.tables_dirty {
            grew |= upload_slice(device, queue, &mut self.model_buf, &self.models);
            grew |= upload_slice(device, queue, &mut self.lod_buf, &self.lods);
            grew |= upload_slice(device, queue, &mut self.section_buf, &self.sections);
            grew |= upload_slice(device, queue, &mut self.section_mat_buf, &self.section_materials);
            self.tables_dirty = false;
        }

        // Instance buffer = static region then the dynamic tail. Ensure capacity for both.
        let static_len = self.static_instances.len() as u32;
        let total = static_len as u64 + self.dynamic.len() as u64;
        let inst_bytes = total.max(1) * std::mem::size_of::<InstanceGpu>() as u64;
        grew |= self.instance_buf.ensure(device, inst_bytes);
        let stride = std::mem::size_of::<InstanceGpu>() as u64;
        if let Some(buf) = self.instance_buf.buf.as_ref() {
            // A grown static region shifts the dynamic tail, so re-upload the whole static
            // region (not just the dirty range) whenever static_len changed.
            let full_static = self.uploaded_static_len != static_len;
            if full_static && static_len > 0 {
                queue.write_buffer(buf, 0, bytemuck::cast_slice(&self.static_instances));
            } else if let Some((lo, hi)) = self.static_dirty {
                let range = &self.static_instances[lo as usize..=hi as usize];
                queue.write_buffer(buf, lo as u64 * stride, bytemuck::cast_slice(range));
            }
            // Dynamic tail every frame at the (possibly new) static offset.
            if !self.dynamic.is_empty() {
                queue.write_buffer(
                    buf,
                    static_len as u64 * stride,
                    bytemuck::cast_slice(&self.dynamic),
                );
            }
        }
        self.static_dirty = None;
        self.uploaded_static_len = static_len;

        // out_args (variant_count * capacity); counters are a fixed buffer allocated up front.
        let args_bytes =
            CULL_VARIANT_COUNT as u64 * self.variant_capacity as u64 * super::INDIRECT_ARG_SIZE;
        if self.out_args_cap < args_bytes || self.out_args.is_none() {
            self.out_args = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_cull_out_args"),
                size: args_bytes,
                usage: wgpu::BufferUsages::INDIRECT
                    | wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            // Records: one per arg slot (same variant partitioning), 8 B each.
            let slots = CULL_VARIANT_COUNT as u64 * self.variant_capacity as u64;
            self.out_records = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_cull_out_records"),
                size: slots * std::mem::size_of::<RecordGpu>() as u64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.out_args_cap = args_bytes;
            grew = true;
        }

        // Finalize + upload params.
        self.params.instance_count = total as u32;
        self.params.variant_capacity = self.variant_capacity;
        self.params.variant_count = CULL_VARIANT_COUNT;
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&self.params));

        if grew || self.bind.is_none() {
            self.rebuild_bind(device);
        }
        grew
    }

    fn rebuild_bind(&mut self, device: &wgpu::Device) {
        let (Some(inst), Some(models), Some(lods), Some(sections), Some(args), Some(records)) = (
            self.instance_buf.buf.as_ref(),
            self.model_buf.buf.as_ref(),
            self.lod_buf.buf.as_ref(),
            self.section_buf.buf.as_ref(),
            self.out_args.as_ref(),
            self.out_records.as_ref(),
        )
        else {
            self.bind = None;
            return;
        };
        self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_cull_bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: inst.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: models.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: lods.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: sections.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: args.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: self.counter_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: records.as_entire_binding(),
                },
            ],
        }));
    }

    // Record the cull dispatch: zero the counters + out_args (so unfilled arg slots stay
    // instance_count = 0 no-op draws), then one thread per instance. No-op until prepare()
    // has run with instances present.
    pub fn dispatch(&self, encoder: &mut wgpu::CommandEncoder) {
        let (Some(bind), Some(args)) = (self.bind.as_ref(), self.out_args.as_ref()) else {
            return;
        };
        if self.params.instance_count == 0 {
            return;
        }
        encoder.clear_buffer(&self.counter_buf, 0, None);
        // Only the args need zeroing for the no-op-slot scheme; records are only ever read
        // at slots the compute filled (their arg has instance_count > 0), so they don't.
        encoder.clear_buffer(args, 0, None);
        let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_cull"),
            timestamp_writes: None,
        });
        cp.set_pipeline(&self.pipeline);
        cp.set_bind_group(0, bind, &[]);
        cp.dispatch_workgroups(self.params.instance_count.div_ceil(64), 1, 1);
    }

    // The GPU-produced indirect args + per-variant counters (consumed by the Stage-3b
    // multi_draw submission).
    pub fn out_args(&self) -> Option<&wgpu::Buffer> {
        self.out_args.as_ref()
    }

    // Buffers the GPU-driven draw pass (3b-2b) reads: the retained instances (VS transform),
    // the per-draw records + per-section materials (VS/FS), and the indirect args.
    pub fn instance_buf(&self) -> Option<&wgpu::Buffer> {
        self.instance_buf.buf.as_ref()
    }

    pub fn out_records(&self) -> Option<&wgpu::Buffer> {
        self.out_records.as_ref()
    }

    // The per-variant counters, doubling as the count buffer for the
    // multi_draw_indexed_indirect_count tail trim (3b-4). Word `v` holds the number of args
    // the compute appended to variant `v`'s partition (clamped to variant_capacity at draw).
    pub fn counter_buf(&self) -> &wgpu::Buffer {
        &self.counter_buf
    }

    pub fn section_material_buf(&self) -> Option<&wgpu::Buffer> {
        self.section_mat_buf.buf.as_ref()
    }

    pub fn variant_capacity(&self) -> u32 {
        self.variant_capacity
    }
}

// Engine-derived per-frame cull + LOD inputs — the REAL values behind Scene::LevelFromDistance2
// (SceneDraw.cpp:572), pushed from C++ via wgr_set_cull_params each frame:
//   objects_z     = ENGINE_CONFIG.objectsZ         — draw distance (distance cull; squared here)
//   lod_scale     = Camera::Left()                 — projection tan(halfFovX); the LOD/sub-pixel
//                                                    `scale` (≈ 0.75 at default FOV, NOT 1)
//   lod_inv_width = Scene::GetLodInvWidth()         — ≈ lodCoef*2/screenWidth (~1e-3, NOT 1);
//                                                    the whole LOD-distance scale rides on this
//   pixel_limit   = 0.125                          — legacy sub-pixel invisibility threshold
#[derive(Clone, Copy)]
pub struct CullInputs {
    pub objects_z: f32,
    pub lod_scale: f32,
    pub lod_inv_width: f32,
    pub pixel_limit: f32,
}

impl Default for CullInputs {
    fn default() -> Self {
        // Inert-safe until C++ pushes the real values. lod_inv_width = 0 makes detail2 = 0 ->
        // always the finest LOD and no sub-pixel cull, so a missing push degrades to "draw
        // everything at full detail within objects_z" — never the ~1e6-too-large resol2 that a
        // value of 1.0 produced (which jumped every model to its coarsest LOD within metres).
        Self {
            objects_z: 900.0,
            lod_scale: 1.0,
            lod_inv_width: 0.0,
            pixel_limit: 0.0,
        }
    }
}

// Build this frame's cull params from the main camera + the engine's LOD inputs. `view` must be
// the engine's camera-relative view (translation zeroed, as PushSceneCamera hands over); all six
// frustum planes are then extracted directly from `proj * view` (see frustum_planes).
pub fn params_from_camera(view: Mat4, proj: Mat4, cam_pos: Vec3, inputs: CullInputs) -> CullParamsGpu {
    let mut p = CullParamsGpu::zeroed();
    p.frustum = frustum_planes(proj * view);
    p.cam_pos = [cam_pos.x, cam_pos.y, cam_pos.z, 0.0];
    p.objects_z2 = inputs.objects_z * inputs.objects_z;
    p.lod_scale = inputs.lod_scale;
    p.lod_inv_width = inputs.lod_inv_width;
    p.pixel_limit = inputs.pixel_limit;
    p
}

// The GPU-driven draw's group-1 layout: instances + records + section materials, all
// read-only storage (docs/gpu-culling-and-depth-plan.md Stage 3b). Groups 0/2/3 (camera,
// bindless textures, sampler array) are shared with the per-draw path.
pub fn gpu_group1_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let storage = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("wgr_gpu_driven_group1"),
        entries: &[storage(0), storage(1), storage(2)],
    })
}

// Build the GPU-driven draw pipelines (gpu_driven.wgsl) — the opaque COLOUR pipeline
// (vs_gpu / fs_gpu) and the depth+normal PREPASS pipeline (vs_gpu / fs_gpu_prepass), sharing
// one shader module + layout. Both: reversed-Z GreaterEqual depth test + WRITE, back-face
// cull. The colour one carries the `linear` HDR override (from the colour format) and one
// pipeline serves solid + alpha-cutout (dynamic per-section alpha_ref discard). The prepass
// one drops shading and writes only the view-space octahedral normal into NORMAL_FORMAT (the
// same G-buffer the per-draw fs_prepass fills), so the GPU-driven set participates in the
// depth+normal prepass (SSAO normals, early-Z) instead of colour-pass only.
#[allow(clippy::too_many_arguments)]
pub fn build_gpu_pipeline(
    device: &wgpu::Device,
    composer: &mut naga_oil::compose::Composer,
    camera_layout: &wgpu::BindGroupLayout,
    group1_layout: &wgpu::BindGroupLayout,
    bindless_layout: &wgpu::BindGroupLayout,
    sampler_layout: &wgpu::BindGroupLayout,
    conform_layout: &wgpu::BindGroupLayout,
    surface_format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::RenderPipeline) {
    let module = crate::shaders::make_module(
        device,
        composer,
        "wgr_gpu_driven",
        include_str!("gpu_driven.wgsl"),
        "gfx3d/gpu_driven.wgsl",
    );
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("wgr_gpu_driven_pipeline_layout"),
        bind_group_layouts: &[
            Some(camera_layout),
            Some(group1_layout),
            Some(bindless_layout),
            Some(sampler_layout),
            // Group 4: terrain-conform heightmap (surface_y/surface_grad), so vs_gpu can
            // conform ClipLand vegetation/fences per vertex — same binding as shader3d.
            Some(conform_layout),
        ],
        immediate_size: 0,
    });
    // pos / norm / uv / conform_sel (locations 0/1/2/5). Same 36-byte WgrMeshVertex stride as
    // the per-draw path; location 5 (the conform selector at byte 32) drives mode-2 conform.
    let vbuf_attrs =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 5 => Uint32];
    let vbuf_layout = wgpu::VertexBufferLayout {
        array_stride: super::BAKED_VERT_SIZE,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &vbuf_attrs,
    };
    let linear = if surface_format == wgpu::TextureFormat::Rgba16Float {
        1.0
    } else {
        0.0
    };
    let constants = [("linear", linear)];
    let depth_stencil = wgpu::DepthStencilState {
        format: super::DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::GreaterEqual), // reversed-Z
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    };
    let primitive = wgpu::PrimitiveState {
        topology: wgpu::PrimitiveTopology::TriangleList,
        front_face: wgpu::FrontFace::Cw,
        cull_mode: Some(wgpu::Face::Back),
        ..Default::default()
    };
    let vbuffers = [vbuf_layout];
    let color = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wgr_gpu_driven_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_gpu"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                ..Default::default()
            },
            buffers: &vbuffers,
        },
        primitive,
        depth_stencil: Some(depth_stencil.clone()),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_gpu"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &constants,
                ..Default::default()
            },
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: None, // opaque
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let prepass = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("wgr_gpu_driven_prepass_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_gpu"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &vbuffers,
        },
        primitive,
        depth_stencil: Some(depth_stencil),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_gpu_prepass"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: super::NORMAL_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    (color, prepass)
}

// Upload a whole CPU slice into a StorageArray, growing it if needed. Returns whether the
// backing buffer moved (so a bind group referencing it must be rebuilt).
fn upload_slice<T: bytemuck::Pod>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    arr: &mut super::StorageArray,
    data: &[T],
) -> bool {
    let bytes = std::mem::size_of_val(data).max(1) as u64;
    let grew = arr.ensure(device, bytes);
    if !data.is_empty() {
        queue.write_buffer(arr.buf.as_ref().unwrap(), 0, bytemuck::cast_slice(data));
    }
    grew
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cull_wgsl_validates() {
        let module =
            naga::front::wgsl::parse_str(include_str!("cull.wgsl")).expect("cull.wgsl parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("cull.wgsl validate");
    }

    fn inside(planes: &[[f32; 4]; 6], p: Vec3) -> bool {
        planes
            .iter()
            .all(|pl| pl[0] * p.x + pl[1] * p.y + pl[2] * p.z + pl[3] >= 0.0)
    }

    // The engine's projection as it actually reaches glam: C++ builds a row-major D3D GfxMatrix
    // (w_clip = +z_view, reversed-Z infinite far with _33 = 1, _43 = -near) and from_cols_array
    // reads it transposed. Columns here = the engine matrix's ROWS.
    fn engine_proj(inv_left: f32, inv_top: f32, near: f32) -> Mat4 {
        Mat4::from_cols(
            Vec4::new(inv_left, 0.0, 0.0, 0.0), // engine row0
            Vec4::new(0.0, inv_top, 0.0, 0.0),  // engine row1
            Vec4::new(0.0, 0.0, 1.0, 1.0),      // engine row2: _33 = 1, _34 = 1
            Vec4::new(0.0, 0.0, -near, 0.0),    // engine row3: _43 = -near, _44 = 0
        )
    }

    // Engine-layout smoke test: with the real row-major D3D projection (w_clip = +z_view), the
    // near plane params_from_camera builds (= row3 of proj*view, the clip.w>=0 half-space) must
    // keep geometry in FRONT of an +X-looking camera and reject what's behind.
    #[test]
    fn params_from_camera_near_plane_faces_forward() {
        // Orthonormal engine-style view basis, forward (Direction, view col 2) = +X.
        let view = Mat4::from_cols(
            Vec4::new(0.0, 0.0, 1.0, 0.0), // aside
            Vec4::new(0.0, 1.0, 0.0, 0.0), // up
            Vec4::new(1.0, 0.0, 0.0, 0.0), // dir = forward = +X
            Vec4::W,
        );
        let proj = engine_proj(1.0, 1.0, 0.1);
        let cam_pos = Vec3::new(5.0, 0.0, 0.0);
        let p = params_from_camera(view, proj, cam_pos, CullInputs::default());

        // Near plane (index 4) points along +X (engine forward), through the camera origin.
        let near = p.frustum[4];
        assert!(near[0] > 0.9, "near normal must be +X (engine forward), got {near:?}");
        let dot = |pl: [f32; 4], v: Vec3| pl[0] * v.x + pl[1] * v.y + pl[2] * v.z + pl[3];
        // Camera-relative (world - cam_pos): in front of +X is inside, behind is out.
        assert!(dot(near, Vec3::new(20.0, 0.0, 0.0) - cam_pos) >= 0.0, "front inside near");
        assert!(dot(near, Vec3::new(-20.0, 0.0, 0.0) - cam_pos) < 0.0, "behind outside near");
    }

    // The decisive guard: every plane from frustum_planes must AGREE with the actual projection,
    // for rotated (yaw+pitch) cameras. A camera-relative point that the projection shows
    // (clip.w > 0 and within the NDC x/y box) must be KEPT — never culled. This catches a wrong
    // near normal (the through-origin near plane is distance-independent, so a bad forward pops
    // geometry in/out purely by look direction — the observed bug) and any swapped/rotated side
    // plane. Also verifies behind-camera and far-to-the-side points ARE culled.
    #[test]
    #[allow(deprecated)] // glam look_at_rh / perspective_infinite_reverse_rh, test-only
    fn frustum_matches_projection_rotated() {
        let proj = Mat4::perspective_infinite_reverse_rh(70f32.to_radians(), 16.0 / 9.0, 0.05);
        for dir in [
            Vec3::new(1.0, 0.3, 0.2),
            Vec3::new(-0.4, 0.8, -1.0),
            Vec3::new(0.2, -0.9, 0.5),
            Vec3::new(-1.0, -0.2, -0.3),
        ] {
            let dir = dir.normalize();
            let mut view = Mat4::look_at_rh(Vec3::ZERO, dir, Vec3::Y);
            view.w_axis = Vec4::new(0.0, 0.0, 0.0, 1.0); // camera-relative (translation zeroed)
            let m = proj * view;
            let planes = frustum_planes(m);

            // Grid of camera-relative points: whatever the projection renders must be kept.
            for gx in -6..=6 {
                for gy in -6..=6 {
                    for gz in 1..=12 {
                        let p = Vec3::new(gx as f32 * 3.0, gy as f32 * 3.0, gz as f32 * 4.0);
                        let clip = m * p.extend(1.0);
                        let ndc_visible = clip.w > 1e-3
                            && clip.x.abs() <= clip.w
                            && clip.y.abs() <= clip.w;
                        if ndc_visible {
                            assert!(
                                inside(&planes, p),
                                "projection-visible point {p:?} wrongly culled (dir {dir:?})"
                            );
                        }
                    }
                }
            }
            // Behind the camera must be culled (near plane must face the right way).
            assert!(!inside(&planes, dir * -10.0), "behind-camera point kept (dir {dir:?})");
            // 90 deg off the view axis (straight out the camera's right) must be culled by a side.
            let right = dir.cross(Vec3::Y).normalize();
            assert!(!inside(&planes, right * 50.0), "side point kept (dir {dir:?})");
        }
    }

    // A reversed-Z, infinite-far perspective (what this backend uses) + a look-at view with
    // the translation ZEROED (the engine's camera-relative convention). The planes are then
    // camera-relative, so points are tested as `world - eye`: the view centre is inside;
    // behind the camera and off to the sides are out.
    #[test]
    #[allow(deprecated)] // glam's look_at_rh / perspective_infinite_reverse_rh, test-only
    fn frustum_planes_classify_points() {
        let eye = Vec3::new(0.0, 0.0, 10.0);
        let target = Vec3::ZERO;
        let up = Vec3::Y;
        let mut view = Mat4::look_at_rh(eye, target, up);
        // Engine convention: geometry is camera-relative, so the view translation is zeroed.
        view.w_axis = Vec4::new(0.0, 0.0, 0.0, 1.0);
        let proj = Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 16.0 / 9.0, 0.1);
        let planes = frustum_planes(proj * view);

        // Points are CAMERA-RELATIVE (world - eye).
        // Straight ahead, well inside the view.
        assert!(inside(&planes, target - eye)); // rel (0,0,-10)
        assert!(inside(&planes, Vec3::new(0.0, 0.0, 5.0) - eye)); // rel (0,0,-5)
        // Behind the camera -> outside the near plane. rel (0,0,10).
        assert!(!inside(&planes, Vec3::new(0.0, 0.0, 20.0) - eye));
        // Far to the side, in front -> outside a side plane. rel (100,0,-10) / (0,100,-10).
        assert!(!inside(&planes, Vec3::new(100.0, 0.0, 0.0) - eye));
        assert!(!inside(&planes, Vec3::new(0.0, 100.0, 0.0) - eye));
        // A distant point straight ahead is NOT culled by the planes (radial distance,
        // not a flat far plane, handles far — the no-op far slot must never reject it).
        assert!(inside(&planes, Vec3::new(0.0, 0.0, -5000.0) - eye));
    }

    // Best-effort headless device; returns None (test skips) when no adapter is available
    // (e.g. CI without a GPU).
    fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn read_u32s(device: &wgpu::Device, queue: &wgpu::Queue, buf: &wgpu::Buffer, len: u64) -> Vec<u32> {
        let bytes = len * 4;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(buf, 0, &staging, 0, bytes);
        queue.submit(std::iter::once(enc.finish()));
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range();
        bytemuck::cast_slice::<u8, u32>(&data).to_vec()
    }

    // End-to-end: register a model, add three instances (one visible, one behind the
    // camera, one past the draw distance), run the compute, and assert exactly the visible
    // one produced an indirect draw for its chosen LOD's section.
    #[test]
    #[allow(deprecated)] // glam look_at_rh / perspective_infinite_reverse_rh, test-only
    fn cull_compute_end_to_end() {
        let Some((device, queue)) = headless() else {
            return;
        };
        let mut cull = CullState::new(&device);

        // 1 model, 2 LODs (finest -> section 0, next -> section 1), variant 0.
        let sections = [
            SectionGpu { first_index: 0, index_count: 3, base_vertex: 0, variant: 0 },
            SectionGpu { first_index: 3, index_count: 3, base_vertex: 0, variant: 0 },
        ];
        let lods = [
            LodGpu { resolution: 0.0, section_base: 0, section_count: 1, is_decal: 0 },
            LodGpu { resolution: 10.0, section_base: 1, section_count: 1, is_decal: 0 },
        ];
        let materials = [SectionMaterialGpu::zeroed(); 2];
        let model = cull.register_model(1.0, &lods, &sections, &materials);

        let mk = |z: f32| InstanceGpu {
            world: Mat4::IDENTITY.to_cols_array(), // cull uses `center`, not `world`
            center: [0.0, 0.0, z, 1.0],
            model,
            flags: 0,
            _pad0: 0,
            _pad1: 0,
        };
        let front = cull.instance_add(mk(0.0)); // 10 units ahead: visible
        let _behind = cull.instance_add(mk(20.0)); // behind the camera at z=10
        let _far = cull.instance_add(mk(-9000.0)); // past the draw distance

        let eye = Vec3::new(0.0, 0.0, 10.0);
        let mut view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y);
        // Camera-relative convention (matches the engine + the compute's `rel` test).
        view.w_axis = Vec4::new(0.0, 0.0, 0.0, 1.0);
        let proj = Mat4::perspective_infinite_reverse_rh(60f32.to_radians(), 1.0, 0.1);
        let mut params = CullParamsGpu::zeroed();
        params.frustum = frustum_planes(proj * view);
        params.cam_pos = [eye.x, eye.y, eye.z, 0.0];
        params.objects_z2 = 100.0 * 100.0;
        params.lod_scale = 1.0;
        params.lod_inv_width = 1.0;
        params.pixel_limit = 0.0; // disable sub-pixel cull for a deterministic result
        cull.set_params(params);

        cull.prepare(&device, &queue);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        cull.dispatch(&mut enc);
        queue.submit(std::iter::once(enc.finish()));

        // resol2 = dist²(=100) * lod_scale²(=1) => level 1 (section 1: first_index 3).
        let words_per_arg = super::ARG_WORDS;
        let total = CULL_VARIANT_COUNT as u64 * cull.variant_capacity() as u64 * words_per_arg;
        let raw = read_u32s(&device, &queue, cull.out_args().unwrap(), total);
        let live: Vec<&[u32]> = raw
            .chunks_exact(words_per_arg as usize)
            .filter(|a| a[1] != 0) // instance_count != 0
            .collect();
        assert_eq!(live.len(), 1, "exactly one instance should draw");
        let a = live[0];
        assert_eq!(a[0], 3, "index_count of the chosen LOD's section");
        assert_eq!(a[1], 1, "instance_count == 1");
        assert_eq!(a[2], 3, "first_index of section 1");
        // first_instance is the record slot; the record resolves to the visible instance
        // and LOD 1's section (global section id 1).
        let rec_slot = a[4] as u64;
        let slots = CULL_VARIANT_COUNT as u64 * cull.variant_capacity() as u64;
        let recs = read_u32s(&device, &queue, cull.out_records().unwrap(), slots * 2);
        assert_eq!(recs[rec_slot as usize * 2], front, "record.instance == visible slot");
        assert_eq!(recs[rec_slot as usize * 2 + 1], 1, "record.section == LOD1 section id");
    }
}
