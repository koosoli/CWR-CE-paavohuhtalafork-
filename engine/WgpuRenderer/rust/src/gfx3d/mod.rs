use rustc_hash::FxHashMap;
use slotmap::{Key, KeyData, SlotMap};
use wgpu::util::DeviceExt;

use crate::ffi::{
    DRAW3D_ON_SURFACE, DRAW3D_ZBIAS_MASK, DRAW3D_ZBIAS_SHIFT, NO_PALETTE, WgrBlend, WgrCamera,
    WgrCmd, WgrCmdKind, WgrDepthMode, WgrDraw3D, WgrMat4, WgrMeshVertex, WgrLight, WgrShadowCaster,
    WgrShadowPass, WgrVec4,
};
use crate::textures::SharedTextures;

// Depth + stencil: the stencil aspect gives per-poly shadow exclusion (a pixel is
// darkened by at most one shadow polygon, so overlapping shadow casters don't
// compound — mirrors GL33's stencil EQUAL 0 / INCR shadow path).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;

// Depth+normal prepass G-buffer target (docs/depth-prepass-plan.md, decision 9): a
// view-space octahedral normal, Rg16Float (compact + banding-free for SSAO/GTAO/SSR).
// Written unconditionally by the prepass; sampled by no consumer yet (Stage 1).
pub const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

// Cascade shadow depth maps: one D32 array layer per cascade.
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MAX_CASCADES: u32 = 4;

// Polygon-offset variants (mirror GL33's SetPolygonOffsetForDecals / ..ForShadows):
// decals nudge coplanar overlays toward the camera; ZBias overlay faces (signs) get
// a stronger, level-scaled push; shadows need a much stronger, angle-independent
// constant bias so ground shadows stay above their surface.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Offset {
    None,
    Decal,
    ZBias(u8), // ZBias level 1..3
    Shadow,
}

// Identifies one 3D render-pipeline variant. Variants are built lazily as draws
// demand new (blend, depth, offset, cutout-threshold) combinations, keyed here so
// identical draws share a pipeline. `alpha_ref`/`is_shadow` are baked as
// pipeline-overridable constants, so the cutout threshold is part of the key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PipelineKey {
    blend: u8,           // WgrBlend
    depth: u8,           // WgrDepthMode
    offset: Offset,      // polygon-offset variant
    alpha_ref_bits: u32, // f32::to_bits of the cutout threshold
    skinned: bool,
    // Colour-pass depth-write override for the depth prepass (decision 2/4): when the
    // prepass already laid down this segment's opaque depth, the colour pass draws the
    // prepassed set GreaterEqual + write-OFF. Same key otherwise, so post-ClearDepth
    // segments (no prepass) still get the write-ON variant.
    depth_write_off: bool,
}

// Identifies one depth+normal prepass pipeline variant (docs/depth-prepass-plan.md,
// decision 3). The prepass only ever draws the opaque set (blend Opaque, offset None,
// depth TestWrite), so blend/depth/offset are fixed and drop out of the key — only the
// VS path (skinned) and the cutout threshold (foliage vs pure opaque) vary.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PrepassKey {
    skinned: bool,
    alpha_ref_bits: u32,
}

// Per-level constant depth-bias magnitude for ZBias overlay faces (signs).
// Tunable live via WGR_ZBIAS_SCALE for z-fight debugging; default 4 per level.
fn zbias_scale() -> f32 {
    env_f32("WGR_ZBIAS_SCALE", 4.0)
}

// OnSurface decal offset magnitude (roads, footprint decals, notebook text).
// Tunable live via WGR_DECAL_SCALE; default 1 → the classic glPolygonOffset(-1,-1).
fn decal_scale() -> f32 {
    env_f32("WGR_DECAL_SCALE", 1.0)
}

pub(crate) fn env_f32(name: &'static str, default: f32) -> f32 {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<&'static str, f32>>> = OnceLock::new();
    let mut map = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    *map.entry(name).or_insert_with(|| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(default)
    })
}

impl PipelineKey {
    fn from_draw(d: &WgrDraw3D, skinned: bool) -> Self {
        let zbias_level = ((d.flags & DRAW3D_ZBIAS_MASK) >> DRAW3D_ZBIAS_SHIFT) as u8;
        let offset = if d.blend == WgrBlend::Shadow {
            Offset::Shadow
        } else if d.flags & DRAW3D_ON_SURFACE != 0 {
            Offset::Decal
        } else if zbias_level > 0 {
            Offset::ZBias(zbias_level)
        } else {
            Offset::None
        };
        PipelineKey {
            blend: d.blend as u8,
            depth: d.depth as u8,
            offset,
            alpha_ref_bits: d.alpha_ref.to_bits(),
            skinned,
            depth_write_off: false,
        }
    }

    // The prepassed opaque set (decision 4): opaque blend, no polygon offset, depth
    // test+write. Foliage (alpha_ref > 0) qualifies; skinned qualifies. Transparents,
    // decals/ZBias and the shadow-darken pass do not. Derives entirely from the key.
    fn prepassed(&self) -> bool {
        self.blend == WgrBlend::Opaque as u8
            && self.offset == Offset::None
            && self.depth == WgrDepthMode::TestWrite as u8
    }

    fn with_write_off(mut self) -> Self {
        self.depth_write_off = true;
        self
    }
}

// The engine's bone-palette cap (MATRIX_4_ARRAY(matrix, 128)); one skinned draw
// occupies this many matrices in the palette pool and in the shader UBO.
const PALETTE_SIZE: usize = 128;

// Per-draw material UBO size: six vec4 (emissive, sun_ambient, sun_diffuse,
// light_diffuse, light_ambient, specular), matching the `Material` struct in
// shader3d.wgsl.
const MATERIAL_SIZE: u64 = 96;

// Fixed capacity of the frame-global light storage buffer (group 0). The
// active count per frame rides in WgrCamera::cam_pos.w; slots beyond it aren't read.
const MAX_LIGHTS: u64 = 256;

// One per-draw entry in the world storage buffer, indexed by @builtin(instance_index).
// The world matrix plus the terrain-conform plane (see WgrDraw3D::conform* and
// the `Object` struct in shader3d.wgsl). Widened from a bare matrix so vegetation can
// upload one shared undeformed mesh and conform per instance in the vertex shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObjectGpu {
    world: WgrMat4,
    conform0: WgrVec4,
    conform1: WgrVec4,
    conform2: WgrVec4,
}

// Per-draw material lighting, uploaded into the group(1)/binding(1) UBO. Fields
// are already folded on the C++ side (WgrDraw3D::mat_*); this is just the GPU
// layout. Only rgb is read by the shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialUbo {
    emissive: [f32; 4],
    sun_ambient: [f32; 4],
    sun_diffuse: [f32; 4],
    light_diffuse: [f32; 4],
    light_ambient: [f32; 4],
    specular: [f32; 4],
}

// Column-major identity, packed into the per-instance world SSBO / ShadowCasterGpu for
// a baked skinned draw: the compute bake already folded the camera-relative world into
// the palette (palette[i] = world * bone[i]), so the rigid pipeline must NOT re-apply it.
const IDENTITY_MAT4: WgrMat4 = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

// One compute-skin-bake dispatch (docs/compute-skin-bake-plan.md): all instances of one
// skinned mesh, baked into `skinned_vbuf` starting at `out_base_vertex`. Phase 1 always
// has instance_count == 1 (one dispatch per distinct palette_slot); Phase 2 flattens
// same-mesh instances into a single dispatch. `palette_base` is the absolute palette
// block of instance 0 (== the draw/caster's palette_slot).
struct BakeGroup {
    mesh: MeshKey,
    palette_base: u32,
    out_base_vertex: u32,
    instance_count: u32,
    vert_count: u32,
}

// Per-dispatch uniform for the skin bake, mirrored by `BakeParams` in skin_bake.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BakeParamsGpu {
    vert_count: u32,
    instance_count: u32,
    palette_base: u32,
    out_base_vertex: u32,
}

// Bytes per baked vertex = one WgrMeshVertex (pos+norm+uv+conform, 9 words).
const BAKED_VERT_SIZE: u64 = std::mem::size_of::<WgrMeshVertex>() as u64;

// Blocking copy of one D32 texture layer into `out` (row 0 = top).
fn read_depth_layer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tex: &wgpu::Texture,
    res: u32,
    layer: u32,
    out: &mut [f32],
) -> bool {
    let res = res as usize;
    let unpadded = (res * 4) as u32;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("wgr_depth_readback"),
        size: padded as u64 * res as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("wgr_depth_readback"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: 0,
                y: 0,
                z: layer,
            },
            aspect: wgpu::TextureAspect::DepthOnly,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(res as u32),
            },
        },
        wgpu::Extent3d {
            width: res as u32,
            height: res as u32,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    if device.poll(wgpu::PollType::wait_indefinitely()).is_err() || !matches!(rx.recv(), Ok(Ok(())))
    {
        return false;
    }
    let data = slice.get_mapped_range();
    for y in 0..res {
        let row = &data[y * padded as usize..y * padded as usize + unpadded as usize];
        out[y * res..(y + 1) * res].copy_from_slice(bytemuck::cast_slice(row));
    }
    drop(data);
    buf.unmap();
    true
}

slotmap::new_key_type! {
    struct MeshKey;
}

struct Mesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    vert_count: u32,
    // Per-vertex skin data (4 bone indices + 4 weights, 8 bytes/vertex); present
    // only for skinned meshes.
    skin: Option<wgpu::Buffer>,
}

struct ShadowTarget {
    tex: wgpu::Texture,
    layer_views: Vec<wgpu::TextureView>,
    sample_view: wgpu::TextureView,
    res: u32,
    layers: u32,
}

struct ShadowPipelines {
    solid: wgpu::RenderPipeline,
    alpha: wgpu::RenderPipeline,
    skin_solid: wgpu::RenderPipeline,
    skin_alpha: wgpu::RenderPipeline,
}

impl ShadowPipelines {
    fn get(&self, skinned: bool, alpha: bool) -> &wgpu::RenderPipeline {
        match (skinned, alpha) {
            (false, false) => &self.solid,
            (false, true) => &self.alpha,
            (true, false) => &self.skin_solid,
            (true, true) => &self.skin_alpha,
        }
    }
}

// One per-caster entry in the shadow-caster storage buffer, indexed by
// @builtin(instance_index) (the draw's base_instance). The cutout threshold is baked as
// a pipeline override (always 0.5), so no alpha_ref rides here and the fragment stage
// never touches this buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowCasterGpu {
    world: WgrMat4,
    conform0: [f32; 4], // x = bcSurfaceY (mode 2)
    conform2: [f32; 4], // z = conform mode (0 = none, 2 = per-vertex ClipLand heightmap)
}

// One instanced draw in a cascade's shadow plan (see prepare_shadows). `repr` is a
// caster index supplying the mesh/section/texture/sampler/skin shared by the bucket;
// `base..base+count` is the base_instance range into the reordered caster SSBO.
struct ShadowBucket {
    repr: u32,
    base: u32,
    count: u32,
}

// Coalesce key for instanceable (non-skinned) shadow casters. Solid casters sample no
// texture, so their texture/sampler are normalized to 0 in the key to let same-mesh
// solids merge regardless of their (unused) material.
#[derive(PartialEq, Eq, Hash)]
struct ShadowBucketKey {
    mesh: u64,
    index_begin: u32,
    index_count: u32,
    alpha: bool,
    texture_id: u64,
    sampler: u32,
}

// group(0) of the shadow depth pass: the light view-projection for one cascade plus
// the camera world position (casters are camera-relative, so surface_y needs cam_pos
// to reconstruct absolute world xz). One per cascade (dynamic offset).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowPassUbo {
    light_vp: WgrMat4,
    cam_pos: [f32; 4],
}

// Group 0 of the lit 3D pipelines: the per-camera UBO (dynamic offset) plus the
// cascade shadow map + comparison sampler. The bind group is recreated when the
// UBO regrows or the shadow target changes (tracked by `shadow_gen`).
struct CameraGroup {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    stride: u64,
    bind_size: u64,
    buf: Option<wgpu::Buffer>,
    cap: u64,
    bind: Option<wgpu::BindGroup>,
    bound_shadow_gen: u64,
    // Frame-global light storage buffer (binding 3). Fixed capacity, created
    // once, so it stays valid across camera-UBO regrowth / shadow-target swaps.
    lights_buf: wgpu::Buffer,
    // Terrain sun-shadow (bindings 4-6): a clamping filter sampler and the
    // world->UV mapping uniform (both created once); the mask texture is owned by
    // Terrain and lent by view, so the bind rebuilds when its generation changes.
    mask_sampler: wgpu::Sampler,
    mapping_buf: wgpu::Buffer,
    bound_mask_gen: u64,
}

impl CameraGroup {
    fn new(device: &wgpu::Device) -> Self {
        let bind_size = std::mem::size_of::<WgrCamera>() as u64;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_3d_camera_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(bind_size),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                // Frame-global point/spot lights, read-only storage. Shared by the
                // lit-mesh + terrain pipelines (terrain reuses this exact layout).
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<crate::ffi::WgrLight>() as u64,
                        ),
                    },
                    count: None,
                },
                // Long-range terrain sun-shadow mask (Rgba16Float, filterable) +
                // its clamping sampler + the world->UV mapping uniform. Written by
                // the terrain compute sweep; sampled here so lit meshes (not just
                // terrain) receive a mountain's cast shadow.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<crate::terrain::TerrainShadowMap>() as u64,
                        ),
                    },
                    count: None,
                },
                // Aerial-perspective froxel volume (3D) + its sampler (the mask sampler
                // is reused for it). Sampled per-fragment by the lit-mesh + terrain
                // fragment shaders (frame::froxel_fog). Owned by Sky, lent by view.
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let lights_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_lights"),
            size: MAX_LIGHTS * std::mem::size_of::<crate::ffi::WgrLight>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Bilinear 2x2 hardware PCF per compare tap; LessEqual = lit when the
        // receiver is at or in front of the stored occluder depth.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_shadow_compare_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_terrain_shadow_mask_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let mapping_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_terrain_shadow_mapping"),
            size: std::mem::size_of::<crate::terrain::TerrainShadowMap>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let align = device
            .limits()
            .min_uniform_buffer_offset_alignment
            .max(bind_size as u32) as u64;
        CameraGroup {
            layout,
            sampler,
            stride: bind_size.div_ceil(align) * align,
            bind_size,
            buf: None,
            cap: 0,
            bind: None,
            bound_shadow_gen: u64::MAX,
            lights_buf,
            mask_sampler,
            mapping_buf,
            bound_mask_gen: u64::MAX,
        }
    }

    // Upload the frame's active lights (clamped to the buffer capacity).
    // The per-camera count travels separately in WgrCamera::cam_pos.w.
    fn upload_lights(&self, queue: &wgpu::Queue, lights: &[crate::ffi::WgrLight]) {
        let n = lights.len().min(MAX_LIGHTS as usize);
        if n > 0 {
            queue.write_buffer(&self.lights_buf, 0, bytemuck::cast_slice(&lights[..n]));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure(
        &mut self,
        device: &wgpu::Device,
        count: usize,
        shadow_view: &wgpu::TextureView,
        shadow_gen: u64,
        mask_view: &wgpu::TextureView,
        mask_gen: u64,
        froxel_view: &wgpu::TextureView,
    ) {
        let needed = count as u64 * self.stride;
        let grow = self.cap < needed || self.buf.is_none();
        if grow {
            let cap = needed.next_power_of_two().max(self.stride * 8);
            self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_3d_camera_ubo"),
                size: cap,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.cap = cap;
        }
        if grow
            || self.bound_shadow_gen != shadow_gen
            || self.bound_mask_gen != mask_gen
            || self.bind.is_none()
        {
            self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wgr_3d_camera_bind"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: self.buf.as_ref().unwrap(),
                            offset: 0,
                            size: wgpu::BufferSize::new(self.bind_size),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(shadow_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.lights_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(mask_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: self.mapping_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(froxel_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                    },
                ],
            }));
            self.bound_shadow_gen = shadow_gen;
            self.bound_mask_gen = mask_gen;
        }
    }

    // Upload the world->UV shadow-mask mapping for this frame (cheap; overwrites).
    fn upload_mapping(&self, queue: &wgpu::Queue, mapping: &crate::terrain::TerrainShadowMap) {
        queue.write_buffer(&self.mapping_buf, 0, bytemuck::bytes_of(mapping));
    }
}

// Holds a dynamic uniform buffer + its bind group, regrown as the frame needs.
struct DynUbo {
    layout: wgpu::BindGroupLayout,
    stride: u64,
    bind_size: u64,
    buf: Option<wgpu::Buffer>,
    bind: Option<wgpu::BindGroup>,
    cap: u64,
    // Distinct buffer label (e.g. "wgr_3d_palette") so per-instance writes are
    // identifiable in a capture instead of a shared "wgr_dyn_ubo".
    label: &'static str,
}

impl DynUbo {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        bind_size: u64,
        visibility: wgpu::ShaderStages,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(bind_size),
                },
                count: None,
            }],
        });
        let align = device
            .limits()
            .min_uniform_buffer_offset_alignment
            .max(bind_size as u32) as u64;
        let stride = bind_size.div_ceil(align) * align;
        DynUbo {
            layout,
            stride,
            bind_size,
            buf: None,
            bind: None,
            cap: 0,
            label,
        }
    }

    // Ensure capacity for `count` entries; (re)create buffer + bind group on
    // growth. Returns true when the buffer was (re)created, so callers that build
    // their own combined bind groups over `buf` know to rebuild them.
    fn ensure(&mut self, device: &wgpu::Device, count: usize) -> bool {
        let needed = count as u64 * self.stride;
        if self.cap >= needed && self.buf.is_some() {
            return false;
        }
        let cap = needed.next_power_of_two().max(self.stride * 64);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size: cap,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_dyn_ubo_bind"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(self.bind_size),
                }),
            }],
        }));
        self.buf = Some(buf);
        self.cap = cap;
        true
    }
}

// Growable read-only storage buffer holding one packed element per draw, uploaded
// with a single `write_buffer` and indexed in-shader by `@builtin(instance_index)`
// (fed as the draw's `base_instance`). This replaces the per-draw dynamic-offset
// UBO uploads — one `write_buffer` per draw — that dominated frame-start
// buffer-copy/barrier traffic, and lays out the per-instance data exactly as
// Stage-3 instancing needs it (a run of instances = a contiguous slot range).
struct StorageArray {
    buf: Option<wgpu::Buffer>,
    cap: u64,
    label: &'static str,
}

impl StorageArray {
    fn new(label: &'static str) -> Self {
        StorageArray {
            buf: None,
            cap: 0,
            label,
        }
    }

    // Ensure capacity for `bytes`; (re)create the buffer on growth. Returns true
    // when the buffer moved, so callers rebuild any bind group that borrows it.
    fn ensure(&mut self, device: &wgpu::Device, bytes: u64) -> bool {
        if self.cap >= bytes && self.buf.is_some() {
            return false;
        }
        let cap = bytes.next_power_of_two().max(4096);
        self.buf = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size: cap,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.cap = cap;
        true
    }
}

// Build a combined group-1 bind group. For the plain pipeline both bindings are
// whole-buffer read-only storage (world @0, material @1), indexed by
// instance_index and bound once per frame. For the skinned pipeline binding 0 is
// instead the dynamic-offset bone palette (`slot0_dynamic` = Some(one-block size)).
fn build_group1_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    slot0: &wgpu::Buffer,
    // Some(one-block size) → dynamic-offset palette (skinned); None → whole-buffer
    // read-only storage (plain world array, indexed by instance_index).
    slot0_dynamic: Option<u64>,
    material: &wgpu::Buffer,
    label: &str,
) -> wgpu::BindGroup {
    let slot0_binding = wgpu::BufferBinding {
        buffer: slot0,
        offset: 0,
        size: slot0_dynamic.and_then(wgpu::BufferSize::new),
    };
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(slot0_binding),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: material.as_entire_binding(),
            },
        ],
    })
}

// Group 4 of the lit mesh pipelines: the terrain heightmap + its sampling params, so
// vs_main can conform ClipLand vegetation to the ground (SurfaceY) per vertex without
// the CPU rewriting the shared mesh. The heightmap is owned by Terrain and lent by
// view; the bind rebuilds when the heightmap generation bumps (realloc on set_heightmap).
// The R32Float heightmap is sampled with textureLoad in the vertex stage (non-filterable),
// exactly like the terrain shader's sample_height (== Landscape::SurfaceY).
struct ConformGroup {
    layout: wgpu::BindGroupLayout,
    params_buf: wgpu::Buffer,
    bind: Option<wgpu::BindGroup>,
    bound_gen: u64,
}

impl ConformGroup {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_3d_conform_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<crate::terrain::TerrainConformParams>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_3d_conform_params"),
            size: std::mem::size_of::<crate::terrain::TerrainConformParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // A 1x1 dummy heightmap so the bind is valid before terrain loads and for the
        // shadow-depth probe. Its params default to enabled=0, so surface_y() is a no-op
        // and rigid (mode 0) draws are unaffected; ensure() swaps in the real heightmap.
        let dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_3d_conform_dummy_hm"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_view = dummy.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_3d_conform_bind_dummy"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        });
        ConformGroup {
            layout,
            params_buf,
            bind: Some(bind),
            bound_gen: u64::MAX,
        }
    }

    // Upload the current params and (re)build the bind group when the heightmap moved
    // (generation bumped) or on first use.
    fn ensure(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        heightmap_view: &wgpu::TextureView,
        generation: u64,
        params: &crate::terrain::TerrainConformParams,
    ) {
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(params));
        if self.bind.is_none() || self.bound_gen != generation {
            self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wgr_3d_conform_bind"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(heightmap_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.params_buf.as_entire_binding(),
                    },
                ],
            }));
            self.bound_gen = generation;
        }
    }
}

pub struct Gfx3d {
    cameras: CameraGroup,
    conform: ConformGroup,
    // Per-draw world matrix, one entry per draw slot, read-only storage indexed by
    // instance_index (the draw's base_instance). Uploaded in a single write_buffer.
    world: StorageArray,
    // Skinned draws: one PALETTE_SIZE-matrix block per slot, dynamic-offset UBO.
    palette: DynUbo,
    // Per-draw material lighting, one entry per draw slot, read-only storage
    // indexed by instance_index. Bound at group(1)/binding(1) for both the plain
    // and skinned group-1 bind groups (combined with world / palette below).
    material: StorageArray,
    // Combined group-1 bind groups: {world|palette @0, material @1}. Rebuilt when
    // any of their backing buffers regrows (tracked via DynUbo::ensure's return).
    group1_plain_layout: wgpu::BindGroupLayout,
    group1_skinned_layout: wgpu::BindGroupLayout,
    group1_plain_bind: Option<wgpu::BindGroup>,
    group1_skinned_bind: Option<wgpu::BindGroup>,

    // Pipeline build inputs, kept so variants can be created lazily as draws
    // demand new (blend, depth, polygon-offset, cutout-threshold) combinations.
    shader: wgpu::ShaderModule,
    plain_layout: wgpu::PipelineLayout,
    skinned_layout: wgpu::PipelineLayout,
    surface_format: wgpu::TextureFormat,
    vbuf_attrs: [wgpu::VertexAttribute; 4],
    skin_attrs: [wgpu::VertexAttribute; 2],
    pipelines: FxHashMap<PipelineKey, wgpu::RenderPipeline>,
    // Depth+normal prepass variants (docs/depth-prepass-plan.md), built lazily
    // alongside the colour pipelines for every opaque draw the frame submits.
    prepass_pipelines: FxHashMap<PrepassKey, wgpu::RenderPipeline>,

    depth: Option<(wgpu::Texture, wgpu::TextureView)>,
    // View-space normal G-buffer, allocated with (and to the same size as) the depth
    // target; the prepass' one colour attachment. Sampled by no consumer yet (Stage 1).
    normal: Option<(wgpu::Texture, wgpu::TextureView)>,
    depth_size: (u32, u32),

    shadow_pass_ubo: DynUbo, // one ShadowPassUbo per cascade
    // Per-caster data as one whole-buffer storage array (indexed by base_instance),
    // uploaded in a single write_buffer — replaces the old per-caster dynamic UBO writes.
    // Laid out per (cascade, bucket) so each bucket's instances are contiguous; a caster
    // in N cascades appears N times (its GPU data is cascade-independent, but bucketing
    // isn't). Built alongside `shadow_plan` in prepare_shadows.
    shadow_caster_ssbo: StorageArray,
    // Per-cascade instanced draw plan over `shadow_caster_ssbo` (built in prepare_shadows,
    // replayed in render_shadow_passes). Indexed [cascade][bucket].
    shadow_plan: Vec<Vec<ShadowBucket>>,
    shadow_caster_layout: wgpu::BindGroupLayout,
    shadow_caster_bind: Option<wgpu::BindGroup>,
    shadow_shader: wgpu::ShaderModule,
    shadow_layout: wgpu::PipelineLayout,
    shadow_skinned_layout: wgpu::PipelineLayout,
    shadow_pipelines: Option<ShadowPipelines>,
    shadow_target: Option<ShadowTarget>,
    // Bumped on shadow-target recreation so the camera bind group refreshes.
    shadow_gen: u64,
    // 1x1 stand-in bound while no shadow map exists (the layout always binds).
    dummy_shadow_view: wgpu::TextureView,

    // Compute skin bake (docs/compute-skin-bake-plan.md). WGR_SKIN_BAKE=0 disables it and
    // falls back to per-pass VS skinning (the skinned pipelines above); default on.
    skin_bake_enabled: bool,
    skin_bake_pipeline: wgpu::ComputePipeline,
    // group(0) = {in_v ro, in_s ro, palette ro, out rw}; rebuilt per dispatch (mesh
    // buffers differ). group(1) = BakeParams (dynamic-offset UBO, one slot per group).
    skin_bake_layout: wgpu::BindGroupLayout,
    skin_bake_params: DynUbo,
    // The whole palette as a flat STORAGE buffer (block b = matrices [b*128..b*128+128)),
    // uploaded once/frame when the bake is on (replaces the fallback dynamic-offset UBO).
    palette_buf: StorageArray,
    // Every baked instance's output verts, base_vertex-addressed; STORAGE (compute writes)
    // + VERTEX (every pass reads). Grow-only.
    skinned_vbuf: Option<wgpu::Buffer>,
    skinned_cap: u64,
    // This frame's bake plan (one dispatch per distinct skinned mesh+pose) and the
    // draw/caster-side lookup palette_slot -> baked base_vertex. Rebuilt in prepare_skin_bake.
    bake_groups: Vec<BakeGroup>,
    skin_base_vertex: FxHashMap<u32, u32>,
    // group(0) skin-bake bind ({vbuf, skin, palette_buf, skinned_vbuf}) cached by mesh.
    // palette_buf/skinned_vbuf are whole-buffer and vbuf/skin are per-mesh-constant, so a
    // mesh's bind is stable frame-to-frame — rebuilt only when palette_buf or skinned_vbuf
    // is (re)allocated (rare growth), evicted on mesh destroy/reskin. This turns the
    // per-frame "one vkUpdateDescriptorSets per skinned mesh" into zero on steady frames.
    bake_bind_cache: FxHashMap<MeshKey, wgpu::BindGroup>,

    meshes: SlotMap<MeshKey, Mesh>,
}

impl Gfx3d {
    pub fn new(
        device: &wgpu::Device,
        textures: &SharedTextures,
        surface_format: wgpu::TextureFormat,
        composer: &mut naga_oil::compose::Composer,
        skin_bake_enabled: bool,
    ) -> Self {
        let shader = crate::shaders::make_module(
            device,
            composer,
            "wgr_3d_shader",
            include_str!("shader3d.wgsl"),
            "gfx3d/shader3d.wgsl",
        );

        // Group 0 = camera UBO + shadow map + comparison sampler. World + material
        // are per-draw storage arrays indexed by instance_index (one upload each);
        // palette is one PALETTE_SIZE-matrix block per skinned draw (dynamic offset).
        let cameras = CameraGroup::new(device);
        let conform = ConformGroup::new(device);
        let world = StorageArray::new("wgr_3d_world_ssbo");
        let palette = DynUbo::new(
            device,
            "wgr_3d_palette_layout",
            (PALETTE_SIZE * std::mem::size_of::<WgrMat4>()) as u64,
            wgpu::ShaderStages::VERTEX,
        );
        let material = StorageArray::new("wgr_3d_material_ssbo");

        // Group 1 for the lit pipelines. Binding 1 (material) is a whole-buffer
        // read-only storage array for both pipelines, indexed by instance_index.
        // Binding 0 differs: the plain pipeline binds the world storage array (also
        // instance-indexed, whole-buffer); the skinned pipeline binds the
        // dynamic-offset bone palette UBO. `slot0_dynamic` selects between them. The
        // shadow-depth pipelines keep their own palette layout, so are unaffected.
        let group1_layout = |label: &str, slot0_dynamic: Option<u64>| {
            let slot0 = match slot0_dynamic {
                Some(size) => wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(size),
                },
                None => wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<ObjectGpu>() as u64),
                },
            };
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: slot0,
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(MATERIAL_SIZE),
                        },
                        count: None,
                    },
                ],
            })
        };
        let group1_plain_layout = group1_layout("wgr_3d_group1_plain_layout", None);
        let group1_skinned_layout = group1_layout(
            "wgr_3d_group1_skinned_layout",
            Some((PALETTE_SIZE * std::mem::size_of::<WgrMat4>()) as u64),
        );

        // Groups 2/3 are the BINDLESS object-texture array + the 8-variant sampler array
        // (docs/bindless-textures-plan.md), bound once for the whole lit-mesh + prepass;
        // the per-instance texture/sampler indices ride the material array. The shadow
        // pipelines keep their own single-texture layouts.
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_3d_pipeline_layout"),
            bind_group_layouts: &[
                Some(&cameras.layout),
                Some(&group1_plain_layout),
                Some(&textures.bindless_layout),
                Some(&textures.sampler_array_layout),
                Some(&conform.layout),
            ],
            immediate_size: 0,
        });
        // Skinned layout swaps the per-draw world matrix (group 1 binding 0) for the
        // bone palette; groups 0/2/3 (camera/textures/samplers) are identical.
        let skinned_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_3d_skinned_pipeline_layout"),
            bind_group_layouts: &[
                Some(&cameras.layout),
                Some(&group1_skinned_layout),
                Some(&textures.bindless_layout),
                Some(&textures.sampler_array_layout),
                Some(&conform.layout),
            ],
            immediate_size: 0,
        });

        // Vertex attributes stored on the struct so pipeline variants can be
        // (re)built lazily; VertexBufferLayout only borrows them at build time.
        // Location 5 = per-vertex terrain-conform selector (0/1/2). Placed at 5 so it
        // never collides with the skinned path's bone/weight attributes (3, 4). Offsets
        // are assigned in order, so it lands at byte 32 (after pos/norm/uv) = conform.
        let vbuf_attrs =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 5 => Uint32];
        // Skin buffer: 8 bytes/vertex — Uint8x4 bone indices + Unorm8x4 weights.
        let skin_attrs = wgpu::vertex_attr_array![3 => Uint8x4, 4 => Unorm8x4];

        let shadow_shader = crate::shaders::make_module(
            device,
            composer,
            "wgr_shadow_depth_shader",
            include_str!("shadow_depth.wgsl"),
            "gfx3d/shadow_depth.wgsl",
        );
        let shadow_pass_ubo = DynUbo::new(
            device,
            "wgr_shadow_pass_layout",
            std::mem::size_of::<ShadowPassUbo>() as u64,
            wgpu::ShaderStages::VERTEX,
        );
        // Per-caster data is a whole-buffer read-only storage array indexed by
        // base_instance (VERTEX only — the fragment bakes its cutout threshold), bound
        // once per pass instead of one dynamic-offset UBO slot per caster.
        let shadow_caster_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("wgr_shadow_caster_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<ShadowCasterGpu>() as u64,
                        ),
                    },
                    count: None,
                }],
            });
        let shadow_caster_ssbo = StorageArray::new("wgr_shadow_caster_ssbo");
        // Group 4 = the terrain heightmap conform group (shared with the lit pipelines),
        // so the depth pass conforms ClipLand vegetation to the same ground per vertex.
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_shadow_pipeline_layout"),
            bind_group_layouts: &[
                Some(&shadow_pass_ubo.layout),
                Some(&shadow_caster_layout),
                Some(&textures.texture_layout),
                Some(&textures.sampler_layout),
                Some(&conform.layout),
            ],
            immediate_size: 0,
        });
        // Skinned depth pipelines swap the caster UBO (group 1) for the bone palette.
        let shadow_skinned_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wgr_shadow_skinned_pipeline_layout"),
                bind_group_layouts: &[
                    Some(&shadow_pass_ubo.layout),
                    Some(&palette.layout),
                    Some(&textures.texture_layout),
                    Some(&textures.sampler_layout),
                    Some(&conform.layout),
                ],
                immediate_size: 0,
            });

        let dummy_shadow = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_shadow_dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_shadow_view = dummy_shadow.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        // Compute skin bake (docs/compute-skin-bake-plan.md). group(0) = the four
        // storage buffers (source verts / skin data / palette / baked output), all
        // whole-buffer so min_binding_size is left open; group(1) = BakeParams.
        let skin_bake_shader = crate::shaders::make_module(
            device,
            composer,
            "wgr_skin_bake_shader",
            include_str!("skin_bake.wgsl"),
            "gfx3d/skin_bake.wgsl",
        );
        // Runtime-sized arrays: min_binding_size is one element (4 B for the u32 vertex/
        // skin/output arrays, 64 B for the mat4 palette). All bound whole-buffer.
        let storage_arr = |read_only: bool, min: u64| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(min),
        };
        let skin_bake_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_skin_bake_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: storage_arr(true, 4),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: storage_arr(true, 4),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: storage_arr(true, 64),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: storage_arr(false, 4),
                    count: None,
                },
            ],
        });
        let skin_bake_params = DynUbo::new(
            device,
            "wgr_skin_bake_params",
            std::mem::size_of::<BakeParamsGpu>() as u64,
            wgpu::ShaderStages::COMPUTE,
        );
        let skin_bake_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wgr_skin_bake_pipeline_layout"),
                bind_group_layouts: &[Some(&skin_bake_layout), Some(&skin_bake_params.layout)],
                immediate_size: 0,
            });
        let skin_bake_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("wgr_skin_bake_pipeline"),
            layout: Some(&skin_bake_pipeline_layout),
            module: &skin_bake_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        Gfx3d {
            cameras,
            conform,
            world,
            palette,
            material,
            group1_plain_layout,
            group1_skinned_layout,
            group1_plain_bind: None,
            group1_skinned_bind: None,
            shader,
            plain_layout: pipeline_layout,
            skinned_layout,
            surface_format,
            vbuf_attrs,
            skin_attrs,
            pipelines: FxHashMap::default(),
            prepass_pipelines: FxHashMap::default(),
            depth: None,
            normal: None,
            depth_size: (0, 0),
            shadow_pass_ubo,
            shadow_caster_ssbo,
            shadow_plan: Vec::new(),
            shadow_caster_layout,
            shadow_caster_bind: None,
            shadow_shader,
            shadow_layout,
            shadow_skinned_layout,
            shadow_pipelines: None,
            shadow_target: None,
            shadow_gen: 0,
            dummy_shadow_view,
            skin_bake_enabled,
            skin_bake_pipeline,
            skin_bake_layout,
            skin_bake_params,
            palette_buf: StorageArray::new("wgr_skin_palette_ssbo"),
            skinned_vbuf: None,
            skinned_cap: 0,
            bake_groups: Vec::new(),
            skin_base_vertex: FxHashMap::default(),
            bake_bind_cache: FxHashMap::default(),
            meshes: SlotMap::with_key(),
        }
    }

    // Blend state for a WgrBlend id (None = opaque, no colour blend).
    fn blend_state(blend: u8) -> Option<wgpu::BlendState> {
        match blend {
            b if b == WgrBlend::Alpha as u8 => Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
            }),
            b if b == WgrBlend::Additive as u8 => Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            }),
            b if b == WgrBlend::Shadow as u8 => Some(wgpu::BlendState {
                // Darken: color = dst*(1-srcA); keep src alpha for the next term.
                // Matches GL33's glBlendFuncSeparate(GL_ZERO, GL_ONE_MINUS_SRC_ALPHA, GL_ONE, GL_ZERO).
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::Zero,
                    operation: wgpu::BlendOperation::Add,
                },
            }),
            _ => None,
        }
    }

    // Create the pipeline for `key` if it doesn't exist yet.
    fn ensure_pipeline(&mut self, device: &wgpu::Device, key: PipelineKey) {
        if self.pipelines.contains_key(&key) {
            return;
        }

        let module = &self.shader;
        let (vs_entry, layout) = if key.skinned {
            ("vs_skinned", &self.skinned_layout)
        } else {
            ("vs_main", &self.plain_layout)
        };

        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<WgrMeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &self.vbuf_attrs,
        };
        let skin_layout = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &self.skin_attrs,
        };
        let plain_buffers = [vbuf_layout.clone()];
        let skinned_buffers = [vbuf_layout, skin_layout];
        let buffers: &[wgpu::VertexBufferLayout] = if key.skinned {
            &skinned_buffers
        } else {
            &plain_buffers
        };

        // WgrDepthMode: 0 none, 1 test (no write), 2 test + write.
        let (test, mut write) = match key.depth {
            0 => (false, false),
            1 => (true, false),
            _ => (true, true),
        };
        // Prepassed opaque draws in the colour pass keep GreaterEqual but stop writing:
        // the prepass already holds the frontmost depth (decision 2). GreaterEqual (not
        // Equal) is robust to any sub-ULP VS drift; write-off just removes redundant writes.
        if key.depth_write_off {
            write = false;
        }
        // Shadows still use DepthBiasState (works well enough for them: drawn
        // depth-test-no-write on the surface). Decal/ZBias overlays instead bias in
        // the vertex shader (depth_bias below) — DepthBiasState's constant term is
        // unreliable / a no-op on float depth formats, so it did nothing for close
        // UI (notebook) and coplanar sign overlays.
        // Reversed-Z: nearer = larger depth, so a toward-camera bias is positive
        // (the forward-Z code used negatives). DepthBiasState's constant term is a
        // no-op on this backend's float depth anyway, but keep the sign correct for
        // formats/GPUs where it does apply.
        let bias = match key.offset {
            Offset::Shadow => wgpu::DepthBiasState {
                constant: 64,
                slope_scale: 1.0,
                clamp: 0.0,
            },
            _ => wgpu::DepthBiasState::default(),
        };

        // Vertex-shader depth bias in [0,1] NDC depth. WGR_DECAL_SCALE / WGR_ZBIAS_SCALE
        // multiply the base unit; ZBias also scales with its 1..3 level.
        const DEPTH_BIAS_UNIT: f32 = 1.0e-5;
        let depth_bias = match key.offset {
            Offset::Decal => decal_scale() * DEPTH_BIAS_UNIT,
            Offset::ZBias(level) => level as f32 * zbias_scale() * DEPTH_BIAS_UNIT,
            _ => 0.0,
        } as f64;

        let alpha_ref = f32::from_bits(key.alpha_ref_bits) as f64;
        let is_shadow = if key.blend == WgrBlend::Shadow as u8 {
            1.0
        } else {
            0.0
        };
        // HDR path: the scene color target is Rgba16Float only when the HDR pipeline
        // is on, so it doubles as the `linear` shading signal (decode + no clamp).
        let linear = if self.surface_format == wgpu::TextureFormat::Rgba16Float {
            1.0
        } else {
            0.0
        };
        let constants = [
            ("alpha_ref", alpha_ref),
            ("is_shadow", is_shadow),
            ("depth_bias", depth_bias),
            ("linear", linear),
        ];

        // Shadow draws exclude already-shadowed pixels via the stencil: test EQUAL
        // 0 (stencil is cleared to 0 each segment; opaque geometry leaves it 0) and
        // INCR on pass, so the first shadow polygon over a pixel darkens it and any
        // overlapping ones fail the test. Non-shadow pipelines leave the stencil
        // untouched (default = disabled).
        let stencil = if key.blend == WgrBlend::Shadow as u8 {
            let face = wgpu::StencilFaceState {
                compare: wgpu::CompareFunction::Equal,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::IncrementClamp,
            };
            wgpu::StencilState {
                front: face,
                back: face,
                read_mask: 0xff,
                write_mask: 0xff,
            }
        } else {
            wgpu::StencilState::default()
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_3d_pipeline"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vs_entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(write),
                depth_compare: Some(if test {
                    // Reversed-Z (see shader3d.wgsl): nearer geometry has the larger
                    // depth value, so the "keep closer" test is GreaterEqual.
                    wgpu::CompareFunction::GreaterEqual
                } else {
                    wgpu::CompareFunction::Always
                }),
                stencil,
                bias,
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.surface_format,
                    blend: Self::blend_state(key.blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.insert(key, pipeline);
    }

    // Create the depth+normal prepass pipeline for `key` if absent. Reuses the colour
    // pass' VS entry + pipeline layout + vertex buffers + override constants (VS parity
    // is load-bearing — see the plan's hazards), writes depth GreaterEqual/write-ON and
    // the view-space normal into NORMAL_FORMAT via fs_prepass. All prepassed draws have
    // offset None, so depth_bias is 0 here, matching their colour VS exactly.
    fn ensure_prepass_pipeline(&mut self, device: &wgpu::Device, key: PrepassKey) {
        if self.prepass_pipelines.contains_key(&key) {
            return;
        }
        let module = &self.shader;
        let (vs_entry, layout) = if key.skinned {
            ("vs_skinned", &self.skinned_layout)
        } else {
            ("vs_main", &self.plain_layout)
        };
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<WgrMeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &self.vbuf_attrs,
        };
        let skin_layout = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &self.skin_attrs,
        };
        let plain_buffers = [vbuf_layout.clone()];
        let skinned_buffers = [vbuf_layout, skin_layout];
        let buffers: &[wgpu::VertexBufferLayout] = if key.skinned {
            &skinned_buffers
        } else {
            &plain_buffers
        };
        let alpha_ref = f32::from_bits(key.alpha_ref_bits) as f64;
        let constants = [("alpha_ref", alpha_ref), ("depth_bias", 0.0)];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_3d_prepass_pipeline"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vs_entry),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                buffers,
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Cw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                // Reversed-Z: nearer geometry has the larger depth value.
                depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs_prepass"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &constants,
                    ..Default::default()
                },
                targets: &[Some(wgpu::ColorTargetState {
                    format: NORMAL_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.prepass_pipelines.insert(key, pipeline);
    }

    pub fn mesh_create(
        &mut self,
        device: &wgpu::Device,
        verts: &[WgrMeshVertex],
        indices: &[u16],
    ) -> u64 {
        if verts.is_empty() || indices.is_empty() {
            return 0;
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_3d_vbuf"),
            contents: bytemuck::cast_slice(verts),
            // STORAGE so the compute skin bake can read it as the rest-pose source
            // (docs/compute-skin-bake-plan.md); non-skinned meshes never bind it there.
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_3d_ibuf"),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let key = self.meshes.insert(Mesh {
            vbuf,
            ibuf,
            index_count: indices.len() as u32,
            vert_count: verts.len() as u32,
            skin: None,
        });
        key.data().as_ffi()
    }

    // Attach interleaved per-vertex skin data (4 bone indices + 4 weights).
    pub fn mesh_set_skin(
        &mut self,
        device: &wgpu::Device,
        handle: u64,
        bones: &[u8],
        weights: &[u8],
    ) {
        let key: MeshKey = KeyData::from_ffi(handle).into();
        let Some(mesh) = self.meshes.get_mut(key) else {
            return;
        };
        let n = mesh.vert_count as usize;
        if bones.len() < n * 4 || weights.len() < n * 4 {
            return;
        }
        // Interleave to 8 bytes/vertex: [b0 b1 b2 b3 w0 w1 w2 w3].
        let mut data = Vec::with_capacity(n * 8);
        for v in 0..n {
            data.extend_from_slice(&bones[v * 4..v * 4 + 4]);
            data.extend_from_slice(&weights[v * 4..v * 4 + 4]);
        }
        mesh.skin = Some(
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("wgr_3d_skin"),
                contents: &data,
                // STORAGE so the compute skin bake reads bones/weights; VERTEX so the
                // fallback VS-skinning path (WGR_SKIN_BAKE=0) can still bind it as attrs.
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::STORAGE,
            }),
        );
        // A cached skin-bake bind would reference the old (or absent) skin buffer.
        self.bake_bind_cache.remove(&key);
    }

    // Re-upload vertex data for an existing (dynamic) mesh, e.g. a skeletally
    // animated character whose vertices are CPU-transformed each frame. The
    // topology (indices) is unchanged; only positions/normals/uvs are rewritten.
    pub fn mesh_update(&mut self, queue: &wgpu::Queue, handle: u64, verts: &[WgrMeshVertex]) {
        let Some(mesh) = self.meshes.get(KeyData::from_ffi(handle).into()) else {
            return;
        };
        if verts.is_empty() || verts.len() as u32 > mesh.vert_count {
            return;
        }
        queue.write_buffer(&mesh.vbuf, 0, bytemuck::cast_slice(verts));
    }

    pub fn mesh_destroy(&mut self, handle: u64) {
        if handle != 0 {
            let key: MeshKey = KeyData::from_ffi(handle).into();
            self.meshes.remove(key);
            // Drop any cached skin-bake bind that referenced this mesh's buffers.
            self.bake_bind_cache.remove(&key);
        }
    }

    // The baked-vertex offset for a skinned draw/caster, or None when the skin bake is
    // off or this slot isn't skinned (docs/compute-skin-bake-plan.md). When Some, the
    // draw/caster routes through the RIGID pipeline with an identity world and this
    // base_vertex into `skinned_vbuf` instead of re-skinning in the VS.
    fn baked_base_vertex(&self, palette_slot: u32) -> Option<u32> {
        if !self.skin_bake_enabled || palette_slot == NO_PALETTE {
            return None;
        }
        self.skin_base_vertex.get(&palette_slot).copied()
    }

    // Build this frame's skin-bake plan (docs/compute-skin-bake-plan.md) from BOTH the
    // color draws and the shadow casters, deduped by palette_slot (1:1 with a mesh+pose),
    // upload the palette as one flat storage buffer, and grow the shared output vertex
    // buffer. Must run BEFORE prepare_shadows + prepare so those pack an identity world
    // for every baked entry. No-op (and clears state) when the bake is disabled.
    pub fn prepare_skin_bake(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        draws: &[WgrDraw3D],
        casters: &[WgrShadowCaster],
        palette: &[WgrMat4],
    ) {
        self.bake_groups.clear();
        self.skin_base_vertex.clear();
        // bake_bind_cache persists across frames (mesh-keyed); it is invalidated only on
        // buffer growth (below) or mesh destroy/reskin, not rebuilt per frame.
        if !self.skin_bake_enabled {
            return;
        }

        // Collect distinct skinned instances by palette_slot. A palette block is one
        // skeleton's skinning, so a slot maps to exactly one (mesh, pose): first-seen wins.
        let mut out_base: u32 = 0;
        let mut consider = |mesh_id: u64, palette_slot: u32, this: &mut Self| {
            if palette_slot == NO_PALETTE {
                return;
            }
            let Some(mesh) = this.meshes.get(KeyData::from_ffi(mesh_id).into()) else {
                return;
            };
            if mesh.skin.is_none() || mesh.vert_count == 0 {
                return;
            }
            if this.skin_base_vertex.contains_key(&palette_slot) {
                return;
            }
            let key = KeyData::from_ffi(mesh_id).into();
            this.skin_base_vertex.insert(palette_slot, out_base);
            this.bake_groups.push(BakeGroup {
                mesh: key,
                palette_base: palette_slot,
                out_base_vertex: out_base,
                instance_count: 1,
                vert_count: mesh.vert_count,
            });
            out_base += mesh.vert_count;
        };
        for d in draws {
            consider(d.mesh, d.palette_slot, self);
        }
        for c in casters {
            consider(c.mesh, c.palette_slot, self);
        }
        if self.bake_groups.is_empty() {
            return;
        }

        // Palette (all blocks) as a flat storage buffer, block b at b*128. Uploaded once
        // here for the compute bake; the fallback VS path's dynamic-offset UBO is skipped
        // in prepare() when the bake is on. `ensure` reports growth so the (whole-buffer)
        // cached binds referencing it can be dropped only when it actually moved.
        let mut buffers_grew = false;
        if !palette.is_empty() {
            buffers_grew |= self
                .palette_buf
                .ensure(device, std::mem::size_of_val(palette) as u64);
            queue.write_buffer(
                self.palette_buf.buf.as_ref().unwrap(),
                0,
                bytemuck::cast_slice(palette),
            );
        }

        // Grow the shared output vertex buffer (STORAGE for the compute write + VERTEX for
        // every pass' read). `out_base` is the total baked vertex count.
        let needed = out_base as u64 * BAKED_VERT_SIZE;
        if self.skinned_cap < needed || self.skinned_vbuf.is_none() {
            let cap = needed.next_power_of_two().max(64 * 1024);
            self.skinned_vbuf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_skinned_vbuf"),
                size: cap,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::VERTEX,
                mapped_at_creation: false,
            }));
            self.skinned_cap = cap;
            buffers_grew = true;
        }
        // A cached group(0) bind pins the specific palette_buf/skinned_vbuf it was built
        // against; when either regrows they are stale, so flush.
        if buffers_grew {
            self.bake_bind_cache.clear();
        }

        // All group params in ONE upload (the dynamic-offset UBO strides entries by
        // min_uniform_buffer_offset_alignment). Building the whole strided region in a
        // scratch buffer and writing it once replaces the per-group write_buffer that
        // showed up in captures as a copy-per-group.
        self.skin_bake_params.ensure(device, self.bake_groups.len());
        let pbuf = self.skin_bake_params.buf.as_ref().unwrap();
        let stride = self.skin_bake_params.stride as usize;
        let mut scratch = vec![0u8; self.bake_groups.len() * stride];
        for (i, g) in self.bake_groups.iter().enumerate() {
            let p = BakeParamsGpu {
                vert_count: g.vert_count,
                instance_count: g.instance_count,
                palette_base: g.palette_base,
                out_base_vertex: g.out_base_vertex,
            };
            let off = i * stride;
            scratch[off..off + std::mem::size_of::<BakeParamsGpu>()]
                .copy_from_slice(bytemuck::bytes_of(&p));
        }
        queue.write_buffer(pbuf, 0, &scratch);

        // Ensure a cached group(0) bind exists for each group's mesh (device available
        // here; skin_bake only records). Whole-buffer palette/output + per-mesh vbuf/skin,
        // so one bind serves every instance of a mesh and persists across frames. Build
        // missing ones into a temp Vec first, then insert — keeps the self borrows disjoint.
        let (Some(pal_buf), Some(out_buf)) =
            (self.palette_buf.buf.as_ref(), self.skinned_vbuf.as_ref())
        else {
            self.bake_groups.clear();
            self.skin_base_vertex.clear();
            return;
        };
        let mut new_binds: Vec<(MeshKey, wgpu::BindGroup)> = Vec::new();
        for g in &self.bake_groups {
            if self.bake_bind_cache.contains_key(&g.mesh)
                || new_binds.iter().any(|(k, _)| *k == g.mesh)
            {
                continue;
            }
            let Some(mesh) = self.meshes.get(g.mesh) else {
                continue;
            };
            let Some(skin) = mesh.skin.as_ref() else {
                continue;
            };
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wgr_skin_bake_bind"),
                layout: &self.skin_bake_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: mesh.vbuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: skin.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: pal_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: out_buf.as_entire_binding(),
                    },
                ],
            });
            new_binds.push((g.mesh, bind));
        }
        for (k, b) in new_binds {
            self.bake_bind_cache.insert(k, b);
        }
    }

    // Record the compute skin-bake pass: one dispatch per BakeGroup, skinning its verts
    // into `skinned_vbuf`. Recorded FIRST in the frame encoder so wgpu's automatic
    // storage->vertex barrier covers every later read (shadows, prepass, forward). No-op
    // when the bake is off or the frame has no skinned geometry.
    pub fn skin_bake(&self, encoder: &mut wgpu::CommandEncoder) {
        if !self.skin_bake_enabled || self.bake_groups.is_empty() {
            return;
        }
        let Some(params_bind) = self.skin_bake_params.bind.as_ref() else {
            return;
        };
        encoder.push_debug_group("wgr_skin_bake");
        let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_skin_bake"),
            timestamp_writes: None,
        });
        cp.set_pipeline(&self.skin_bake_pipeline);
        // group(0) is per-mesh (cached); only rebind it when the mesh changes from the
        // previous dispatch. group(1) is the same buffer at a per-group dynamic offset —
        // a cheap offset rebind, not a descriptor update.
        let mut last_mesh: Option<MeshKey> = None;
        for (i, g) in self.bake_groups.iter().enumerate() {
            let Some(bind) = self.bake_bind_cache.get(&g.mesh) else {
                continue;
            };
            if last_mesh != Some(g.mesh) {
                cp.set_bind_group(0, bind, &[]);
                last_mesh = Some(g.mesh);
            }
            cp.set_bind_group(1, params_bind, &[(i as u64 * self.skin_bake_params.stride) as u32]);
            let threads = g.vert_count * g.instance_count;
            cp.dispatch_workgroups(threads.div_ceil(64), 1, 1);
        }
        drop(cp);
        encoder.pop_debug_group();
    }

    // (Re)create the depth target to match the surface
    pub fn ensure_depth(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if self.depth_size == size && self.depth.is_some() {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_3d_depth"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth = Some((texture, view));
        // View-space normal G-buffer, matched to the depth size. TEXTURE_BINDING now
        // (harmless) so Stage 2 can expose it to SSAO without a realloc.
        let normal = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_3d_normal"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: NORMAL_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let normal_view = normal.create_view(&wgpu::TextureViewDescriptor::default());
        self.normal = Some((normal, normal_view));
        self.depth_size = size;
    }

    pub fn depth_view(&self) -> Option<&wgpu::TextureView> {
        self.depth.as_ref().map(|(_, v)| v)
    }

    // The prepass' view-space normal G-buffer view (the prepass colour attachment).
    pub fn normal_view(&self) -> Option<&wgpu::TextureView> {
        self.normal.as_ref().map(|(_, v)| v)
    }

    // The cascade shadow depth map as a D2Array view (or the 1x1 dummy when no shadows),
    // lent to the froxel fill so it can occlude the fog by objects + terrain (god rays).
    pub fn shadow_sample_view(&self) -> &wgpu::TextureView {
        self.shadow_target
            .as_ref()
            .map(|t| &t.sample_view)
            .unwrap_or(&self.dummy_shadow_view)
    }

    // Group-0 (camera UBO + shadow map) layout, shared with the terrain pipeline so
    // terrain reuses the world camera and shadow resources.
    pub fn camera_layout(&self) -> &wgpu::BindGroupLayout {
        &self.cameras.layout
    }

    // Camera bind group for the current frame (valid after `prepare`); index a
    // camera by `slot * camera_stride()` as the dynamic offset.
    pub fn camera_bind(&self) -> Option<&wgpu::BindGroup> {
        self.cameras.bind.as_ref()
    }

    pub fn camera_stride(&self) -> u64 {
        self.cameras.stride
    }

    fn ensure_shadow_target(&mut self, device: &wgpu::Device, res: u32, layers: u32) {
        if let Some(t) = &self.shadow_target {
            if t.res == res && t.layers == layers {
                return;
            }
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_shadow_map"),
            size: wgpu::Extent3d {
                width: res,
                height: res,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let layer_views = (0..layers)
            .map(|l| {
                tex.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("wgr_shadow_layer"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: l,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let sample_view = tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("wgr_shadow_sample"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        self.shadow_target = Some(ShadowTarget {
            tex,
            layer_views,
            sample_view,
            res,
            layers,
        });
        self.shadow_gen += 1;
    }

    fn ensure_shadow_pipelines(&mut self, device: &wgpu::Device) {
        if self.shadow_pipelines.is_some() {
            return;
        }
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<WgrMeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &self.vbuf_attrs,
        };
        let skin_layout = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &self.skin_attrs,
        };
        let plain_buffers = [vbuf_layout.clone()];
        let skinned_buffers = [vbuf_layout, skin_layout];
        let build = |vs: &str, fs: Option<&str>, skinned: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("wgr_shadow_depth_pipeline"),
                layout: Some(if skinned {
                    &self.shadow_skinned_layout
                } else {
                    &self.shadow_layout
                }),
                vertex: wgpu::VertexState {
                    module: &self.shadow_shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers: if skinned {
                        &skinned_buffers
                    } else {
                        &plain_buffers
                    },
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Cw,
                    // No culling: single-sided walls/roofs must still cast.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: SHADOW_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState {
                        constant: 4,
                        slope_scale: 2.5,
                        clamp: 0.0,
                    },
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: fs.map(|entry| wgpu::FragmentState {
                    module: &self.shadow_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        self.shadow_pipelines = Some(ShadowPipelines {
            solid: build("vs_solid", None, false),
            alpha: build("vs_alpha", Some("fs_alpha"), false),
            skin_solid: build("vs_skin_solid", None, true),
            skin_alpha: build("vs_skin_alpha", Some("fs_skin_alpha"), true),
        });
    }

    pub fn prepare_shadows(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &WgrShadowPass,
        casters: &[WgrShadowCaster],
    ) {
        let count = pass.count.min(MAX_CASCADES);
        if count == 0 || casters.is_empty() || pass.resolution == 0 {
            return;
        }
        self.ensure_shadow_target(device, pass.resolution, count);
        self.ensure_shadow_pipelines(device);

        self.shadow_pass_ubo.ensure(device, count as usize);
        let buf = self.shadow_pass_ubo.buf.as_ref().unwrap();
        for c in 0..count as usize {
            let entry = ShadowPassUbo {
                light_vp: pass.light_vp[c],
                cam_pos: pass.cam_pos,
            };
            queue.write_buffer(
                buf,
                c as u64 * self.shadow_pass_ubo.stride,
                bytemuck::bytes_of(&entry),
            );
        }

        // Bucket casters per cascade into instanced draws (mirrors plan_3d for the color
        // pass). Depth-only casters are all order-independent, so within a cascade we
        // coalesce non-skinned casters by (mesh, section, alpha, texture, sampler) and
        // pack their GPU data contiguously — one instanced draw per bucket instead of one
        // draw per caster. Skinned casters can't instance (per-caster palette offset), so
        // each is its own count-1 bucket. The packed array is laid out per (cascade,
        // bucket); a caster in several cascades is packed once per cascade (its data is
        // cascade-independent, but its bucket position isn't).
        let mut caster_gpu: Vec<ShadowCasterGpu> = Vec::with_capacity(casters.len());
        self.shadow_plan.clear();
        let mut bucket_index: FxHashMap<ShadowBucketKey, usize> = FxHashMap::default();
        for c in 0..count as usize {
            // Buckets in first-seen order + their member caster indices.
            let mut buckets: Vec<(u32, Vec<u32>)> = Vec::new();
            bucket_index.clear();
            let mut cascade: Vec<ShadowBucket> = Vec::new();
            for (i, caster) in casters.iter().enumerate() {
                if caster.cascade_mask & (1 << c) == 0 || caster.index_count == 0 {
                    continue;
                }
                let Some(mesh) = self.meshes.get(KeyData::from_ffi(caster.mesh).into()) else {
                    continue;
                };
                if caster.index_begin + caster.index_count > mesh.index_count {
                    continue;
                }
                let alpha = caster.alpha_ref > 0.0;
                // Baked casters (docs/compute-skin-bake-plan.md) route through the RIGID
                // depth pipeline reading skinned_vbuf; `skinned` = the VS-skinning fallback.
                let baked = if mesh.skin.is_some() {
                    self.baked_base_vertex(caster.palette_slot)
                } else {
                    None
                };
                let skinned =
                    caster.palette_slot != NO_PALETTE && mesh.skin.is_some() && baked.is_none();
                if skinned {
                    // Skinned casters read the bone palette, not the SSBO; base_instance is
                    // unused by their shader, but pack an entry so the array stays dense.
                    let base = caster_gpu.len() as u32;
                    caster_gpu.push(ShadowCasterGpu {
                        world: caster.world,
                        conform0: caster.conform0,
                        conform2: caster.conform2,
                    });
                    cascade.push(ShadowBucket {
                        repr: i as u32,
                        base,
                        count: 1,
                    });
                } else if baked.is_some() {
                    // Baked caster: the rigid vs_solid/vs_alpha reads casters[instance].world,
                    // so pack an IDENTITY world (the pose's world is folded into the palette,
                    // already applied by the bake). Each pose is unique, so it can't coalesce
                    // with the rigid mesh's buffer — its own count-1 bucket.
                    let base = caster_gpu.len() as u32;
                    caster_gpu.push(ShadowCasterGpu {
                        world: IDENTITY_MAT4,
                        conform0: [0.0; 4],
                        conform2: [0.0; 4],
                    });
                    cascade.push(ShadowBucket {
                        repr: i as u32,
                        base,
                        count: 1,
                    });
                } else {
                    let key = ShadowBucketKey {
                        mesh: caster.mesh,
                        index_begin: caster.index_begin,
                        index_count: caster.index_count,
                        alpha,
                        texture_id: if alpha { caster.texture_id } else { 0 },
                        sampler: if alpha { caster.sampler.0 } else { 0 },
                    };
                    let bi = *bucket_index.entry(key).or_insert_with(|| {
                        buckets.push((i as u32, Vec::new()));
                        buckets.len() - 1
                    });
                    buckets[bi].1.push(i as u32);
                }
            }
            // Flush the cascade's coalesced buckets: pack each contiguously.
            for (repr, members) in buckets {
                let base = caster_gpu.len() as u32;
                let bcount = members.len() as u32;
                for &mi in &members {
                    let caster = &casters[mi as usize];
                    caster_gpu.push(ShadowCasterGpu {
                        world: caster.world,
                        conform0: caster.conform0,
                        conform2: caster.conform2,
                    });
                }
                cascade.push(ShadowBucket {
                    repr,
                    base,
                    count: bcount,
                });
            }
            self.shadow_plan.push(cascade);
        }
        // Upload the whole reordered array in ONE write_buffer (indexed in-shader by
        // base_instance) — replaces the per-caster dynamic-UBO write loop.
        let grew = self
            .shadow_caster_ssbo
            .ensure(device, std::mem::size_of_val(caster_gpu.as_slice()) as u64);
        let buf = self.shadow_caster_ssbo.buf.as_ref().unwrap();
        if !caster_gpu.is_empty() {
            queue.write_buffer(buf, 0, bytemuck::cast_slice(&caster_gpu));
        }
        if grew || self.shadow_caster_bind.is_none() {
            self.shadow_caster_bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wgr_shadow_caster_bind"),
                layout: &self.shadow_caster_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            }));
        }
    }

    pub fn render_shadow_passes(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        textures: &SharedTextures,
        pass: &WgrShadowPass,
        casters: &[WgrShadowCaster],
    ) {
        let count = pass.count.min(MAX_CASCADES);
        if count == 0 || casters.is_empty() {
            return;
        }
        let (Some(target), Some(pipes), Some(pass_bind), Some(caster_bind), Some(conform_bind)) = (
            self.shadow_target.as_ref(),
            self.shadow_pipelines.as_ref(),
            self.shadow_pass_ubo.bind.as_ref(),
            self.shadow_caster_bind.as_ref(),
            self.conform.bind.as_ref(),
        ) else {
            return;
        };

        for c in 0..count.min(target.layers) as usize {
            let Some(plan) = self.shadow_plan.get(c) else {
                continue;
            };
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_shadow_cascade"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.layer_views[c],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Group 0 (pass UBO, this cascade's light-VP) shares one layout across every
            // shadow pipeline and is bound first, so a later pipeline switch never
            // invalidates it — set it once per cascade instead of per draw.
            rp.set_bind_group(0, pass_bind, &[(c as u64 * self.shadow_pass_ubo.stride) as u32]);

            for bucket in plan {
                let caster = &casters[bucket.repr as usize];
                let Some(mesh) = self.meshes.get(KeyData::from_ffi(caster.mesh).into()) else {
                    continue;
                };
                let alpha = caster.alpha_ref > 0.0;
                // Baked casters route through the rigid pipeline reading skinned_vbuf at
                // base_vertex (identity world in the SSBO); `skin` = the VS-skinning fallback.
                let baked = if caster.palette_slot != NO_PALETTE {
                    self.baked_base_vertex(caster.palette_slot)
                } else {
                    None
                };
                let skin = if caster.palette_slot != NO_PALETTE && baked.is_none() {
                    mesh.skin.as_ref()
                } else {
                    None
                };
                rp.set_pipeline(pipes.get(skin.is_some(), alpha));
                if let Some(skin) = skin {
                    let Some(palette_bind) = self.palette.bind.as_ref() else {
                        continue;
                    };
                    rp.set_bind_group(
                        1,
                        palette_bind,
                        &[(caster.palette_slot as u64 * self.palette.stride) as u32],
                    );
                    rp.set_vertex_buffer(1, skin.slice(..));
                } else {
                    // Whole-buffer storage bound once; each instance's slot travels as
                    // base_instance (read via @builtin(instance_index)) — no dynamic offset.
                    rp.set_bind_group(1, caster_bind, &[]);
                }
                rp.set_bind_group(
                    2,
                    textures.texture_bind(if alpha { caster.texture_id } else { 0 }),
                    &[],
                );
                rp.set_bind_group(3, textures.sampler_bind(caster.sampler.index()), &[]);
                rp.set_bind_group(4, conform_bind, &[]);
                // Baked casters pull rest-of-space verts from the shared skinned buffer at
                // base_vertex; rigid/VS-skinned from the mesh's own. Index buffer unchanged.
                let (vbuf, base_vertex) = match baked {
                    Some(bv) if self.skinned_vbuf.is_some() => {
                        (self.skinned_vbuf.as_ref().unwrap(), bv as i32)
                    }
                    _ => (&mesh.vbuf, 0),
                };
                rp.set_vertex_buffer(0, vbuf.slice(..));
                rp.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
                rp.draw_indexed(
                    caster.index_begin..(caster.index_begin + caster.index_count),
                    base_vertex,
                    bucket.base..(bucket.base + bucket.count),
                );
            }
        }
    }

    // Synchronous readback of one cascade layer, row 0 = top; returns the
    // resolution, or 0 when unavailable.
    pub fn shadow_map_read(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer: u32,
        out: &mut [f32],
    ) -> u32 {
        let Some(target) = self.shadow_target.as_ref() else {
            return 0;
        };
        if layer >= target.layers || out.len() < (target.res * target.res) as usize {
            return 0;
        }
        if read_depth_layer(device, queue, &target.tex, target.res, layer, out) {
            target.res
        } else {
            0
        }
    }

    // Render a caller-supplied triangle soup with the solid shadow depth
    // pipeline into a scratch map and read it back (row 0 = top). Validates the
    // GPU depth path against the ShadowMath::CpuRasterDepth CPU reference.
    #[allow(clippy::too_many_arguments)]
    pub fn shadow_depth_probe(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures: &SharedTextures,
        light_vp: &[f32; 16],
        verts_xyz: &[f32],
        res: u32,
        out: &mut [f32],
    ) -> bool {
        let vert_count = verts_xyz.len() / 3;
        if vert_count == 0 || !vert_count.is_multiple_of(3) || vert_count > u16::MAX as usize {
            return false;
        }
        if out.len() < (res * res) as usize {
            return false;
        }
        self.ensure_shadow_pipelines(device);
        let pipes = self.shadow_pipelines.as_ref().unwrap();

        let verts: Vec<WgrMeshVertex> = verts_xyz
            .chunks_exact(3)
            .map(|p| WgrMeshVertex {
                pos: glam::Vec3::new(p[0], p[1], p[2]),
                norm: glam::Vec3::ZERO,
                uv: glam::Vec2::ZERO,
                conform: 0,
            })
            .collect();
        let indices: Vec<u16> = (0..vert_count as u16).collect();
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_probe_vbuf"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_probe_ibuf"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        self.shadow_pass_ubo.ensure(device, 1);
        let pass_entry = ShadowPassUbo {
            light_vp: *light_vp,
            cam_pos: [0.0; 4],
        };
        queue.write_buffer(
            self.shadow_pass_ubo.buf.as_ref().unwrap(),
            0,
            bytemuck::bytes_of(&pass_entry),
        );
        let identity = ShadowCasterGpu {
            world: {
                let mut m = [0.0f32; 16];
                m[0] = 1.0;
                m[5] = 1.0;
                m[10] = 1.0;
                m[15] = 1.0;
                m
            },
            conform0: [0.0; 4],
            conform2: [0.0; 4], // mode 0: probe triangle is rigid, no conform
        };
        self.shadow_caster_ssbo
            .ensure(device, std::mem::size_of::<ShadowCasterGpu>() as u64);
        let caster_buf = self.shadow_caster_ssbo.buf.as_ref().unwrap();
        queue.write_buffer(caster_buf, 0, bytemuck::bytes_of(&identity));
        let caster_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_probe_caster_bind"),
            layout: &self.shadow_caster_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: caster_buf.as_entire_binding(),
            }],
        });

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_probe_depth"),
            size: wgpu::Extent3d {
                width: res,
                height: res,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("wgr_probe"),
        });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_probe_pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&pipes.solid);
            rp.set_bind_group(0, self.shadow_pass_ubo.bind.as_ref().unwrap(), &[0]);
            rp.set_bind_group(1, &caster_bind, &[]);
            rp.set_bind_group(2, textures.texture_bind(0), &[]);
            rp.set_bind_group(3, textures.sampler_bind(0), &[]);
            rp.set_bind_group(4, self.conform.bind.as_ref().unwrap(), &[]);
            rp.set_vertex_buffer(0, vbuf.slice(..));
            rp.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint16);
            rp.draw_indexed(0..vert_count as u32, 0, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));

        read_depth_layer(device, queue, &tex, res, 0, out)
    }

    // Upload cameras, per-draw world matrices, and the skinned-draw bone palette;
    // regrow the dynamic UBOs. `palette` is a flat pool of PALETTE_SIZE-matrix
    // blocks, one per palette slot (world already pre-multiplied in on the C++ side).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        // Resolves each draw's texture handle to its bindless slot for packing into the
        // per-instance material (the fragment shader indexes the bindless arrays with it).
        textures: &SharedTextures,
        cameras: &[WgrCamera],
        draws: &[WgrDraw3D],
        // Slot -> draws index, the instancing plan's upload order (see plan_3d): a
        // bucket's instances occupy contiguous slots so one instanced draw covers them.
        // The per-draw storage arrays (world, material) are packed in this order and
        // read in-shader by @builtin(instance_index) == base_instance == slot.
        order: &[u32],
        palette: &[WgrMat4],
        lights: &[WgrLight],
        shadow_mask_view: &wgpu::TextureView,
        shadow_mask_gen: u64,
        shadow_mapping: &crate::terrain::TerrainShadowMap,
        heightmap_view: &wgpu::TextureView,
        heightmap_gen: u64,
        conform_params: &crate::terrain::TerrainConformParams,
        froxel_view: &wgpu::TextureView,
    ) {
        // Lend the terrain heightmap + its sampling params to the mesh conform group
        // (group 4) so vs_main can conform ClipLand vegetation to SurfaceY per vertex.
        self.conform
            .ensure(device, queue, heightmap_view, heightmap_gen, conform_params);
        // Frame-global lights into the group-0 storage buffer (shared with
        // terrain via the camera bind group). The per-camera count is in cam_pos.w.
        self.cameras.upload_lights(queue, lights);
        // Terrain sun-shadow world->UV mapping for the lit-mesh sampler (group 0).
        self.cameras.upload_mapping(queue, shadow_mapping);
        if !cameras.is_empty() {
            // Bind the current shadow map (or the dummy while none exists); the
            // depth passes for this frame were prepared before this call, so the
            // target is final.
            let shadow_view = self
                .shadow_target
                .as_ref()
                .map(|t| &t.sample_view)
                .unwrap_or(&self.dummy_shadow_view);
            self.cameras.ensure(
                device,
                cameras.len(),
                shadow_view,
                self.shadow_gen,
                shadow_mask_view,
                shadow_mask_gen,
                froxel_view,
            );
            let buf = self.cameras.buf.as_ref().unwrap();
            for (i, c) in cameras.iter().enumerate() {
                queue.write_buffer(buf, i as u64 * self.cameras.stride, bytemuck::bytes_of(c));
            }
        }
        // Track buffer regrowth so the combined group-1 bind groups (which borrow
        // these buffers) are rebuilt only when one actually moved.
        let mut world_grew = false;
        let mut palette_grew = false;
        let mut material_grew = false;
        if !order.is_empty() {
            // Pack the whole frame's world matrices + materials contiguously, one upload each,
            // indexed in-shader by instance_index. The pack order is the instancing plan's slot
            // order (not raw draw order): a bucket's instances land in a contiguous slot range so
            // one draw covers them all.
            //
            // write_buffer_with hands us a WRITE-ONLY view of wgpu's staging memory (it may be
            // write-combined, so reads are disallowed). We build each struct straight into that view
            // via into_chunks + write_iter — no intermediate scratch Vec and no second memcpy, which
            // is what plain write_buffer would cost (build a Vec, then memcpy it into staging).
            // obj_bytes/mat_bytes are exact multiples of the element size, so the chunk remainder is
            // always empty.
            let n = order.len();
            // A baked skinned draw routes through the rigid pipeline: its world is folded
            // into the palette (baked position is camera-relative world space already), so
            // pack an identity world + no conform so vs_main leaves the baked verts alone.
            let bake_on = self.skin_bake_enabled;
            let base_map = &self.skin_base_vertex;
            const OBJ_SZ: usize = std::mem::size_of::<ObjectGpu>();
            let obj_bytes = (n * OBJ_SZ) as u64;
            world_grew = self.world.ensure(device, obj_bytes);
            if let Some(sz) = wgpu::BufferSize::new(obj_bytes) {
                let mut view = queue
                    .write_buffer_with(self.world.buf.as_ref().unwrap(), 0, sz)
                    .expect("world staging view");
                let (chunks, _rem) = view.slice(..).into_chunks::<OBJ_SZ>();
                chunks.write_iter(order.iter().map(|&i| {
                    let d = &draws[i as usize];
                    let baked = bake_on
                        && d.palette_slot != NO_PALETTE
                        && base_map.contains_key(&d.palette_slot);
                    let obj = if baked {
                        ObjectGpu {
                            world: IDENTITY_MAT4,
                            conform0: [0.0; 4],
                            conform1: [0.0; 4],
                            conform2: [0.0; 4],
                        }
                    } else {
                        ObjectGpu {
                            world: d.world,
                            conform0: d.conform0,
                            conform1: d.conform1,
                            conform2: d.conform2,
                        }
                    };
                    bytemuck::cast::<ObjectGpu, [u8; OBJ_SZ]>(obj)
                }));
            }

            const MAT_SZ: usize = std::mem::size_of::<MaterialUbo>();
            let mat_bytes = (n * MAT_SZ) as u64;
            material_grew = self.material.ensure(device, mat_bytes);
            if let Some(sz) = wgpu::BufferSize::new(mat_bytes) {
                let mut view = queue
                    .write_buffer_with(self.material.buf.as_ref().unwrap(), 0, sz)
                    .expect("material staging view");
                let (chunks, _rem) = view.slice(..).into_chunks::<MAT_SZ>();
                chunks.write_iter(order.iter().map(|&i| {
                    let d = &draws[i as usize];
                    // Pack the bindless indices into the material's spare emissive.w
                    // (only emissive.rgb is read for shading): (tex_slot << 3) | sampler.
                    // The fragment shader unpacks it to index the bindless texture +
                    // sampler arrays, so texture/sampler need no per-draw bind and drop
                    // out of the instancing key (plan_3d).
                    let slot = textures.texture_slot(d.texture_id);
                    let packed = (slot << 3) | (d.sampler.index() as u32 & 0x7);
                    let mut emissive = d.mat_emissive;
                    emissive[3] = f32::from_bits(packed);
                    bytemuck::cast::<MaterialUbo, [u8; MAT_SZ]>(MaterialUbo {
                        emissive,
                        sun_ambient: d.mat_sun_ambient,
                        sun_diffuse: d.mat_sun_diffuse,
                        light_diffuse: d.mat_light_diffuse,
                        light_ambient: d.mat_light_ambient,
                        specular: d.mat_specular,
                    })
                }));
            }
        }
        // One dynamic-UBO slot per PALETTE_SIZE-matrix block. A block is exactly
        // the UBO bind size, so slot s lives at s * stride. Skipped when the compute skin
        // bake is on: it uploads the palette as a storage buffer (prepare_skin_bake) and
        // no draw uses the VS-skinning path, so this UBO would be dead weight.
        let slots = palette.len() / PALETTE_SIZE;
        if slots > 0 && !self.skin_bake_enabled {
            palette_grew = self.palette.ensure(device, slots);
            let buf = self.palette.buf.as_ref().unwrap();
            for s in 0..slots {
                let block = &palette[s * PALETTE_SIZE..(s + 1) * PALETTE_SIZE];
                queue.write_buffer(
                    buf,
                    s as u64 * self.palette.stride,
                    bytemuck::cast_slice(block),
                );
            }
        }

        // (Re)build the combined group-1 bind groups when a backing buffer moved
        // (or on first use). The plain bind needs the world + material buffers;
        // the skinned bind the palette + material buffers.
        if let (Some(world_buf), Some(mat_buf)) =
            (self.world.buf.as_ref(), self.material.buf.as_ref())
        {
            if world_grew || material_grew || self.group1_plain_bind.is_none() {
                self.group1_plain_bind = Some(build_group1_bind(
                    device,
                    &self.group1_plain_layout,
                    world_buf,
                    None,
                    mat_buf,
                    "wgr_3d_group1_plain_bind",
                ));
            }
        }
        if let (Some(pal_buf), Some(mat_buf)) =
            (self.palette.buf.as_ref(), self.material.buf.as_ref())
        {
            if palette_grew || material_grew || self.group1_skinned_bind.is_none() {
                self.group1_skinned_bind = Some(build_group1_bind(
                    device,
                    &self.group1_skinned_layout,
                    pal_buf,
                    Some(self.palette.bind_size),
                    mat_buf,
                    "wgr_3d_group1_skinned_bind",
                ));
            }
        }

        // Build any pipeline variants this frame's draws need before the render
        // pass records them (pipeline creation needs &mut self; draw_one is &self).
        for d in draws {
            let has_skin = self
                .meshes
                .get(KeyData::from_ffi(d.mesh).into())
                .is_some_and(|m| m.skin.is_some());
            // A baked draw draws through the RIGID pipeline (identity world + baked verts),
            // so build the plain variant for it, not the skinned one.
            let baked = self.baked_base_vertex(d.palette_slot).is_some() && has_skin;
            let skinned = d.palette_slot != NO_PALETTE && has_skin && !baked;
            let key = PipelineKey::from_draw(d, skinned);
            self.ensure_pipeline(device, key);
            // Opaque draws are prepassed: also build their colour write-off variant (used
            // in the prepassed segment) and their depth+normal prepass pipeline.
            if key.prepassed() {
                self.ensure_pipeline(device, key.with_write_off());
                self.ensure_prepass_pipeline(
                    device,
                    PrepassKey {
                        skinned,
                        alpha_ref_bits: key.alpha_ref_bits,
                    },
                );
            }
        }
    }

    // Build the frame's instancing plan from the command stream. Consecutive
    // "standard opaque" 3D draws (blend Opaque, no polygon offset, depth test+write,
    // not skinned) are order-independent, so within a maximal run of them we bucket by
    // (mesh, section, texture, sampler, camera, pipeline): every bucket collapses to
    // one instanced draw whose instances read their own world/conform/material from the
    // per-draw storage arrays via @builtin(instance_index). The upload order (`order`)
    // lays each bucket's instances in a contiguous slot range so base_instance covers
    // them. Anything not instanceable (transparent, decal/ZBias, skinned, non-standard
    // depth) is a barrier: it flushes the run and is emitted as a count-1 draw in place,
    // preserving draw order across it. Terrain / 2D / ClearDepth also flush and pass
    // through, so the plan mirrors the stream's ordering exactly.
    pub fn plan_3d(&self, cmds: &[WgrCmd], draws: &[WgrDraw3D]) -> Plan3d {
        // slot -> draws index (the storage-array pack order).
        let mut order: Vec<u32> = Vec::with_capacity(cmds.len());
        let mut ops: Vec<Plan3dOp> = Vec::with_capacity(cmds.len());
        // The current reorderable run: buckets in first-seen order + their members
        // (draws indices), and a key->bucket map to coalesce.
        let mut buckets: Vec<(u32, Vec<u32>)> = Vec::new();
        let mut bucket_index: FxHashMap<BucketKey, usize> = FxHashMap::default();

        // Emit the current run's buckets (each becomes one instanced draw over a
        // contiguous slot range) and reset it. Called at every barrier.
        fn flush_run(
            order: &mut Vec<u32>,
            ops: &mut Vec<Plan3dOp>,
            buckets: &mut Vec<(u32, Vec<u32>)>,
            bucket_index: &mut FxHashMap<BucketKey, usize>,
        ) {
            for (repr, members) in buckets.drain(..) {
                let base = order.len() as u32;
                let count = members.len() as u32;
                order.extend_from_slice(&members);
                ops.push(Plan3dOp::Draw3D {
                    draw: repr,
                    base,
                    count,
                });
            }
            bucket_index.clear();
        }

        for cmd in cmds {
            if cmd.kind == WgrCmdKind::Draw3D as u32 {
                let Some(d) = draws.get(cmd.arg as usize) else {
                    continue;
                };
                // Drops draws that render nothing (missing/invalid mesh, empty range),
                // exactly as draw_one would bail on them — they never reach the GPU, so
                // omitting them from the plan (and their storage slot) is equivalent.
                if d.index_count == 0 {
                    continue;
                }
                let Some(mesh) = self.meshes.get(KeyData::from_ffi(d.mesh).into()) else {
                    continue;
                };
                if d.index_begin + d.index_count > mesh.index_count {
                    continue;
                }
                let skinned = d.palette_slot != NO_PALETTE && mesh.skin.is_some();
                let pkey = PipelineKey::from_draw(d, false);
                let instanceable = !skinned
                    && d.blend == WgrBlend::Opaque
                    && pkey.offset == Offset::None
                    && d.depth == WgrDepthMode::TestWrite;
                if instanceable {
                    let key = BucketKey {
                        mesh: d.mesh,
                        index_begin: d.index_begin,
                        index_count: d.index_count,
                        camera: d.camera,
                        pipeline: pkey,
                    };
                    let bi = *bucket_index.entry(key).or_insert_with(|| {
                        buckets.push((cmd.arg, Vec::new()));
                        buckets.len() - 1
                    });
                    buckets[bi].1.push(cmd.arg);
                } else {
                    // Barrier draw: keep its position by flushing the run first, then
                    // emit it standalone (its own slot, count 1).
                    flush_run(&mut order, &mut ops, &mut buckets, &mut bucket_index);
                    let base = order.len() as u32;
                    order.push(cmd.arg);
                    ops.push(Plan3dOp::Draw3D {
                        draw: cmd.arg,
                        base,
                        count: 1,
                    });
                }
            } else if cmd.kind == WgrCmdKind::Draw2D as u32 {
                flush_run(&mut order, &mut ops, &mut buckets, &mut bucket_index);
                ops.push(Plan3dOp::Draw2D(cmd.arg));
            } else if cmd.kind == WgrCmdKind::DrawTerrain as u32 {
                flush_run(&mut order, &mut ops, &mut buckets, &mut bucket_index);
                ops.push(Plan3dOp::Terrain(cmd.arg));
            } else if cmd.kind == WgrCmdKind::DrawWater as u32 {
                flush_run(&mut order, &mut ops, &mut buckets, &mut bucket_index);
                ops.push(Plan3dOp::Water(cmd.arg));
            } else if cmd.kind == WgrCmdKind::ClearDepth as u32 {
                flush_run(&mut order, &mut ops, &mut buckets, &mut bucket_index);
                ops.push(Plan3dOp::ClearDepth);
            } else if cmd.kind == WgrCmdKind::Resolve as u32 {
                flush_run(&mut order, &mut ops, &mut buckets, &mut bucket_index);
                ops.push(Plan3dOp::Resolve);
            }
        }
        flush_run(&mut order, &mut ops, &mut buckets, &mut bucket_index);
        Plan3d { order, ops }
    }

    // Issue one (possibly instanced) indexed draw. `d` supplies the mesh, section,
    // texture, sampler, camera and pipeline shared by every instance; `base..base+count`
    // is the base_instance range, selecting each instance's world/conform/material slot
    // in the prepared storage arrays via @builtin(instance_index). `st` carries the
    // bind/pipeline/buffer state already set on `pass` so redundant re-binds are
    // skipped — three of the five bind groups (camera, world/material, conform) are
    // frame-constant, so within a run of 3D draws they are set once, not per draw.
    // The caller resets `st` (Pass3dState::default) whenever another pipeline runs on
    // the pass (terrain/2D) or a new pass begins, since that invalidates this state.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_one(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        textures: &SharedTextures,
        d: &WgrDraw3D,
        base: u32,
        count: u32,
        st: &mut Pass3dState,
        mode: Pass3dMode,
    ) {
        if d.index_count == 0 {
            return;
        }
        let Some(camera_bind) = self.cameras.bind.as_ref() else {
            return;
        };
        let Some(conform_bind) = self.conform.bind.as_ref() else {
            return;
        };
        let Some(mesh) = self.meshes.get(KeyData::from_ffi(d.mesh).into()) else {
            return;
        };
        if d.index_begin + d.index_count > mesh.index_count {
            return;
        }
        // Compute skin bake (docs/compute-skin-bake-plan.md): a baked skinned draw routes
        // through the RIGID pipeline (identity world packed in prepare) reading the shared
        // baked vertex buffer at `base_vertex`, so it is NOT `skinned` for pipeline/bind
        // selection. Only the true VS-skinning fallback (WGR_SKIN_BAKE=0) sets `skinned`.
        let baked = if mesh.skin.is_some() {
            self.baked_base_vertex(d.palette_slot)
        } else {
            None
        };
        if baked.is_some() && self.skinned_vbuf.is_none() {
            return;
        }
        let skinned = d.palette_slot != NO_PALETTE && mesh.skin.is_some() && baked.is_none();
        let base_key = PipelineKey::from_draw(d, skinned);
        // Pipeline by pass mode. Prepass draws ONLY the opaque set (self-filter here so
        // the caller can replay a whole segment); the colour pass flips the same set to
        // its write-off variant when the prepass already laid its depth (decision 2/4).
        let pipeline = match mode {
            Pass3dMode::Prepass => {
                if !base_key.prepassed() {
                    return;
                }
                let Some(p) = self.prepass_pipelines.get(&PrepassKey {
                    skinned,
                    alpha_ref_bits: base_key.alpha_ref_bits,
                }) else {
                    return;
                };
                p
            }
            Pass3dMode::Color { depth_write_off } => {
                let key = if depth_write_off && base_key.prepassed() {
                    base_key.with_write_off()
                } else {
                    base_key
                };
                let Some(p) = self.pipelines.get(&key) else {
                    return;
                };
                p
            }
        };
        // Bail (without touching `st`) if the group-1 backing the draw needs isn't ready.
        if skinned && self.group1_skinned_bind.is_none() {
            return;
        }
        if !skinned && self.group1_plain_bind.is_none() {
            return;
        }

        // A plain<->skinned switch changes the group-1 pipeline layout, which invalidates
        // bind groups 1..=4 in wgpu; drop their tracked state so they are re-bound.
        if st.last_skinned.is_some() && st.last_skinned != Some(skinned) {
            st.group1_plain = false;
            st.skinned_off = None;
            st.bindless = false;
            st.conform = false;
        }
        st.last_skinned = Some(skinned);

        let pipe_id = pipeline as *const wgpu::RenderPipeline as usize;
        if st.pipeline != Some(pipe_id) {
            pass.set_pipeline(pipeline);
            st.pipeline = Some(pipe_id);
        }

        // Group 0: camera UBO (dynamic offset). One camera for the whole 3D pass in
        // practice, so this binds once.
        let cam_off = (d.camera as u64 * self.cameras.stride) as u32;
        if st.cam_off != Some(cam_off) {
            pass.set_bind_group(0, camera_bind, &[cam_off]);
            st.cam_off = Some(cam_off);
        }

        // Group 1: plain = whole-buffer world/material (frame-constant, bind once);
        // skinned = bone palette at a per-draw dynamic offset.
        if skinned {
            let off = (d.palette_slot as u64 * self.palette.stride) as u32;
            if st.skinned_off != Some(off) {
                pass.set_bind_group(1, self.group1_skinned_bind.as_ref().unwrap(), &[off]);
                st.skinned_off = Some(off);
            }
            st.group1_plain = false;
            pass.set_vertex_buffer(1, mesh.skin.as_ref().unwrap().slice(..));
        } else if !st.group1_plain {
            pass.set_bind_group(1, self.group1_plain_bind.as_ref().unwrap(), &[]);
            st.group1_plain = true;
            st.skinned_off = None;
        }

        // Groups 2 (bindless object textures) + 3 (8-variant sampler array) are
        // frame-constant, bound once per run — the per-instance texture/sampler indices
        // ride the material array, so these are no longer per-draw.
        if !st.bindless {
            pass.set_bind_group(2, textures.bindless_bind(), &[]);
            pass.set_bind_group(3, textures.sampler_array_bind(), &[]);
            st.bindless = true;
        }

        // Group 4: conform heightmap (frame-constant, bind once).
        if !st.conform {
            pass.set_bind_group(4, conform_bind, &[]);
            st.conform = true;
        }

        // Vertex buffer at slot 0: baked draws pull from the shared skinned output buffer
        // at `base_vertex`; rigid/VS-skinned draws pull from the mesh's own buffer. The
        // index buffer is always the mesh's (indices are unchanged by the bake). Skip
        // re-binding when the same source buffer repeats back-to-back.
        let (vertex_buf, base_vertex) = match baked {
            Some(bv) => (self.skinned_vbuf.as_ref().unwrap(), bv as i32),
            None => (&mesh.vbuf, 0),
        };
        let vbuf_id = vertex_buf as *const wgpu::Buffer as usize;
        if st.vbuf != Some(vbuf_id) {
            pass.set_vertex_buffer(0, vertex_buf.slice(..));
            st.vbuf = Some(vbuf_id);
        }
        let ibuf_id = &mesh.ibuf as *const wgpu::Buffer as usize;
        if st.ibuf != Some(ibuf_id) {
            pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
            st.ibuf = Some(ibuf_id);
        }

        pass.draw_indexed(
            d.index_begin..(d.index_begin + d.index_count),
            base_vertex,
            base..(base + count),
        );
    }
}

// Coalesce key for instanceable draws: two draws merge into one instanced draw only
// when every field draw_one reads from the WgrDraw3D (other than the per-instance
// world/conform/material, which ride the storage arrays) is identical. Texture + sampler
// are NOT here: they're bindless (indexed per-instance from the material), so same-mesh
// draws with different textures/samplers merge into one instanced draw.
#[derive(PartialEq, Eq, Hash)]
struct BucketKey {
    mesh: u64,
    index_begin: u32,
    index_count: u32,
    camera: u32,
    pipeline: PipelineKey,
}

// One replayable step in the instancing plan (see plan_3d). Draw3D carries the repr
// draw (mesh/section/texture/pipeline) plus the base_instance range of its instances.
pub enum Plan3dOp {
    ClearDepth,
    Draw2D(u32),  // batch index
    Terrain(u32), // terrain batch index
    Water(u32),   // water batch index
    Draw3D { draw: u32, base: u32, count: u32 },
    // Scene->UI seam: tonemap the HDR target to the swapchain; ops after this are
    // display-referred UI (drawn straight to the swapchain).
    Resolve,
}

// The frame's instancing plan: `order[slot]` = the draws index whose world/material
// packs into that storage slot (base_instance), and `ops` replays the stream with 3D
// runs collapsed into instanced draws. Ownership is separate from Gfx3d so it can be
// held across the &mut prepare() borrow and consumed in the render loop.
pub struct Plan3d {
    pub order: Vec<u32>,
    pub ops: Vec<Plan3dOp>,
}

// Which pass draw_one records into (docs/depth-prepass-plan.md). Prepass = the
// depth+normal G-buffer (opaque set only, self-filtered). Color = the shading pass;
// `depth_write_off` is set for the prepassed segment so its opaque set draws
// GreaterEqual/write-off over the already-complete depth.
#[derive(Clone, Copy)]
pub enum Pass3dMode {
    Prepass,
    Color { depth_write_off: bool },
}

// Bind/pipeline/buffer state already set on a render pass, so draw_one can skip
// redundant re-binds across a run of 3D draws. Reset (Default) whenever another
// pipeline runs on the pass (terrain/2D) or a new render pass begins — both invalidate
// everything tracked here.
#[derive(Default)]
pub struct Pass3dState {
    pipeline: Option<usize>,   // last render pipeline (pointer identity)
    last_skinned: Option<bool>,
    cam_off: Option<u32>,
    group1_plain: bool,        // plain group-1 (world/material) currently bound
    skinned_off: Option<u32>,  // skinned group-1 palette offset currently bound
    bindless: bool,            // groups 2/3 (bindless textures + sampler array) bound
    conform: bool,             // group-4 conform heightmap currently bound
    vbuf: Option<usize>,       // vertex buffer at slot 0 (pointer identity)
    ibuf: Option<usize>,       // index buffer (pointer identity)
}
