use std::collections::HashMap;

use slotmap::{Key, KeyData, SlotMap};
use wgpu::util::DeviceExt;

use crate::ffi::{
    DRAW3D_ON_SURFACE, DRAW3D_ZBIAS_MASK, DRAW3D_ZBIAS_SHIFT, NO_PALETTE, WgrBlend, WgrCamera,
    WgrDraw3D, WgrMat4, WgrMeshVertex, WgrShadowCaster, WgrShadowPass,
};
use crate::textures::SharedTextures;

// Depth + stencil: the stencil aspect gives per-poly shadow exclusion (a pixel is
// darkened by at most one shadow polygon, so overlapping shadow casters don't
// compound — mirrors GL33's stencil EQUAL 0 / INCR shadow path).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;

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

fn env_f32(name: &'static str, default: f32) -> f32 {
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
        }
    }
}

// The engine's bone-palette cap (MATRIX_4_ARRAY(matrix, 128)); one skinned draw
// occupies this many matrices in the palette pool and in the shader UBO.
const PALETTE_SIZE: usize = 128;

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowCasterUbo {
    world: WgrMat4,
    params: [f32; 4], // x = alpha_ref
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
            ],
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
        }
    }

    fn ensure(
        &mut self,
        device: &wgpu::Device,
        count: usize,
        shadow_view: &wgpu::TextureView,
        shadow_gen: u64,
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
        if grow || self.bound_shadow_gen != shadow_gen || self.bind.is_none() {
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
                ],
            }));
            self.bound_shadow_gen = shadow_gen;
        }
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
}

impl DynUbo {
    fn new(
        device: &wgpu::Device,
        label: &str,
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
        }
    }

    // Ensure capacity for `count` entries; (re)create buffer + bind group on growth.
    fn ensure(&mut self, device: &wgpu::Device, count: usize) {
        let needed = count as u64 * self.stride;
        if self.cap >= needed && self.buf.is_some() {
            return;
        }
        let cap = needed.next_power_of_two().max(self.stride * 64);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_dyn_ubo"),
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
    }
}

pub struct Gfx3d {
    cameras: CameraGroup,
    world: DynUbo,
    // Skinned draws: one PALETTE_SIZE-matrix block per slot, dynamic-offset UBO.
    palette: DynUbo,

    // Pipeline build inputs, kept so variants can be created lazily as draws
    // demand new (blend, depth, polygon-offset, cutout-threshold) combinations.
    shader: wgpu::ShaderModule,
    plain_layout: wgpu::PipelineLayout,
    skinned_layout: wgpu::PipelineLayout,
    surface_format: wgpu::TextureFormat,
    vbuf_attrs: [wgpu::VertexAttribute; 3],
    skin_attrs: [wgpu::VertexAttribute; 2],
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,

    depth: Option<(wgpu::Texture, wgpu::TextureView)>,
    depth_size: (u32, u32),

    shadow_pass_ubo: DynUbo,   // one light_vp per cascade
    shadow_caster_ubo: DynUbo, // one ShadowCasterUbo per caster
    shadow_shader: wgpu::ShaderModule,
    shadow_layout: wgpu::PipelineLayout,
    shadow_skinned_layout: wgpu::PipelineLayout,
    shadow_pipelines: Option<ShadowPipelines>,
    shadow_target: Option<ShadowTarget>,
    // Bumped on shadow-target recreation so the camera bind group refreshes.
    shadow_gen: u64,
    // 1x1 stand-in bound while no shadow map exists (the layout always binds).
    dummy_shadow_view: wgpu::TextureView,

    meshes: SlotMap<MeshKey, Mesh>,
}

impl Gfx3d {
    pub fn new(
        device: &wgpu::Device,
        textures: &SharedTextures,
        surface_format: wgpu::TextureFormat,
        composer: &mut naga_oil::compose::Composer,
    ) -> Self {
        let shader = crate::shaders::make_module(
            device,
            composer,
            "wgr_3d_shader",
            include_str!("shader3d.wgsl"),
            "gfx3d/shader3d.wgsl",
        );

        // Group 0 = camera UBO + shadow map + comparison sampler; world holds one
        // 64-byte entry per draw, palette one PALETTE_SIZE-matrix block per
        // skinned draw (both vertex-only, dynamic offset).
        let cameras = CameraGroup::new(device);
        let world = DynUbo::new(
            device,
            "wgr_3d_world_layout",
            64,
            wgpu::ShaderStages::VERTEX,
        );
        let palette = DynUbo::new(
            device,
            "wgr_3d_palette_layout",
            (PALETTE_SIZE * std::mem::size_of::<WgrMat4>()) as u64,
            wgpu::ShaderStages::VERTEX,
        );

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_3d_pipeline_layout"),
            bind_group_layouts: &[
                Some(&cameras.layout),
                Some(&world.layout),
                Some(&textures.texture_layout),
                Some(&textures.sampler_layout),
            ],
            immediate_size: 0,
        });
        // Skinned layout swaps the per-draw world matrix (group 1) for the bone
        // palette; groups 0/2/3 (camera/texture/sampler) are identical.
        let skinned_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_3d_skinned_pipeline_layout"),
            bind_group_layouts: &[
                Some(&cameras.layout),
                Some(&palette.layout),
                Some(&textures.texture_layout),
                Some(&textures.sampler_layout),
            ],
            immediate_size: 0,
        });

        // Vertex attributes stored on the struct so pipeline variants can be
        // (re)built lazily; VertexBufferLayout only borrows them at build time.
        let vbuf_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];
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
            64,
            wgpu::ShaderStages::VERTEX,
        );
        let shadow_caster_ubo = DynUbo::new(
            device,
            "wgr_shadow_caster_layout",
            std::mem::size_of::<ShadowCasterUbo>() as u64,
            wgpu::ShaderStages::VERTEX_FRAGMENT,
        );
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_shadow_pipeline_layout"),
            bind_group_layouts: &[
                Some(&shadow_pass_ubo.layout),
                Some(&shadow_caster_ubo.layout),
                Some(&textures.texture_layout),
                Some(&textures.sampler_layout),
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

        Gfx3d {
            cameras,
            world,
            palette,
            shader,
            plain_layout: pipeline_layout,
            skinned_layout,
            surface_format,
            vbuf_attrs,
            skin_attrs,
            pipelines: HashMap::new(),
            depth: None,
            depth_size: (0, 0),
            shadow_pass_ubo,
            shadow_caster_ubo,
            shadow_shader,
            shadow_layout,
            shadow_skinned_layout,
            shadow_pipelines: None,
            shadow_target: None,
            shadow_gen: 0,
            dummy_shadow_view,
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
        let (test, write) = match key.depth {
            0 => (false, false),
            1 => (true, false),
            _ => (true, true),
        };
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
        let constants = [
            ("alpha_ref", alpha_ref),
            ("is_shadow", is_shadow),
            ("depth_bias", depth_bias),
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
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
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
        let Some(mesh) = self.meshes.get_mut(KeyData::from_ffi(handle).into()) else {
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
                usage: wgpu::BufferUsages::VERTEX,
            }),
        );
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
            self.meshes.remove(KeyData::from_ffi(handle).into());
        }
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
        self.depth_size = size;
    }

    pub fn depth_view(&self) -> Option<&wgpu::TextureView> {
        self.depth.as_ref().map(|(_, v)| v)
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
            queue.write_buffer(
                buf,
                c as u64 * self.shadow_pass_ubo.stride,
                bytemuck::bytes_of(&pass.light_vp[c]),
            );
        }

        self.shadow_caster_ubo.ensure(device, casters.len());
        let buf = self.shadow_caster_ubo.buf.as_ref().unwrap();
        for (i, caster) in casters.iter().enumerate() {
            let entry = ShadowCasterUbo {
                world: caster.world,
                params: [caster.alpha_ref, 0.0, 0.0, 0.0],
            };
            queue.write_buffer(
                buf,
                i as u64 * self.shadow_caster_ubo.stride,
                bytemuck::bytes_of(&entry),
            );
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
        let (Some(target), Some(pipes), Some(pass_bind), Some(caster_bind)) = (
            self.shadow_target.as_ref(),
            self.shadow_pipelines.as_ref(),
            self.shadow_pass_ubo.bind.as_ref(),
            self.shadow_caster_ubo.bind.as_ref(),
        ) else {
            return;
        };

        for c in 0..count.min(target.layers) as usize {
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
                let skin = if caster.palette_slot != NO_PALETTE {
                    mesh.skin.as_ref()
                } else {
                    None
                };
                rp.set_pipeline(pipes.get(skin.is_some(), alpha));
                rp.set_bind_group(
                    0,
                    pass_bind,
                    &[(c as u64 * self.shadow_pass_ubo.stride) as u32],
                );
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
                    rp.set_bind_group(
                        1,
                        caster_bind,
                        &[(i as u64 * self.shadow_caster_ubo.stride) as u32],
                    );
                }
                rp.set_bind_group(
                    2,
                    textures.texture_bind(if alpha { caster.texture_id } else { 0 }),
                    &[],
                );
                rp.set_bind_group(3, textures.sampler_bind(caster.sampler.index()), &[]);
                rp.set_vertex_buffer(0, mesh.vbuf.slice(..));
                rp.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
                rp.draw_indexed(
                    caster.index_begin..(caster.index_begin + caster.index_count),
                    0,
                    0..1,
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
        queue.write_buffer(
            self.shadow_pass_ubo.buf.as_ref().unwrap(),
            0,
            bytemuck::cast_slice(light_vp),
        );
        self.shadow_caster_ubo.ensure(device, 1);
        let identity = ShadowCasterUbo {
            world: {
                let mut m = [0.0f32; 16];
                m[0] = 1.0;
                m[5] = 1.0;
                m[10] = 1.0;
                m[15] = 1.0;
                m
            },
            params: [0.0; 4],
        };
        queue.write_buffer(
            self.shadow_caster_ubo.buf.as_ref().unwrap(),
            0,
            bytemuck::bytes_of(&identity),
        );

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
            rp.set_bind_group(1, self.shadow_caster_ubo.bind.as_ref().unwrap(), &[0]);
            rp.set_bind_group(2, textures.texture_bind(0), &[]);
            rp.set_bind_group(3, textures.sampler_bind(0), &[]);
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
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cameras: &[WgrCamera],
        draws: &[WgrDraw3D],
        palette: &[WgrMat4],
    ) {
        if !cameras.is_empty() {
            // Bind the current shadow map (or the dummy while none exists); the
            // depth passes for this frame were prepared before this call, so the
            // target is final.
            let shadow_view = self
                .shadow_target
                .as_ref()
                .map(|t| &t.sample_view)
                .unwrap_or(&self.dummy_shadow_view);
            self.cameras
                .ensure(device, cameras.len(), shadow_view, self.shadow_gen);
            let buf = self.cameras.buf.as_ref().unwrap();
            for (i, c) in cameras.iter().enumerate() {
                queue.write_buffer(buf, i as u64 * self.cameras.stride, bytemuck::bytes_of(c));
            }
        }
        if !draws.is_empty() {
            self.world.ensure(device, draws.len());
            let buf = self.world.buf.as_ref().unwrap();
            for (i, d) in draws.iter().enumerate() {
                queue.write_buffer(
                    buf,
                    i as u64 * self.world.stride,
                    bytemuck::bytes_of(&d.world),
                );
            }
        }
        // One dynamic-UBO slot per PALETTE_SIZE-matrix block. A block is exactly
        // the UBO bind size, so slot s lives at s * stride.
        let slots = palette.len() / PALETTE_SIZE;
        if slots > 0 {
            self.palette.ensure(device, slots);
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

        // Build any pipeline variants this frame's draws need before the render
        // pass records them (pipeline creation needs &mut self; draw_one is &self).
        for d in draws {
            let has_skin = self
                .meshes
                .get(KeyData::from_ffi(d.mesh).into())
                .is_some_and(|m| m.skin.is_some());
            let skinned = d.palette_slot != NO_PALETTE && has_skin;
            self.ensure_pipeline(device, PipelineKey::from_draw(d, skinned));
        }
    }

    // Issue one indexed draw. `index` is the draw's slot in the prepared arrays,
    // selecting its world matrix; `d.camera` selects its camera.
    pub fn draw_one(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        textures: &SharedTextures,
        d: &WgrDraw3D,
        index: u32,
    ) {
        if d.index_count == 0 {
            return;
        }
        let (Some(camera_bind), Some(world_bind)) =
            (self.cameras.bind.as_ref(), self.world.bind.as_ref())
        else {
            return;
        };
        let Some(mesh) = self.meshes.get(KeyData::from_ffi(d.mesh).into()) else {
            return;
        };
        if d.index_begin + d.index_count > mesh.index_count {
            return;
        }

        let camera_off = &[(d.camera as u64 * self.cameras.stride) as u32];
        let index_range = d.index_begin..(d.index_begin + d.index_count);

        // Skinned path: needs a palette slot AND the mesh to carry skin data.
        // Group 1 becomes the bone palette; groups 0/2/3 are shared.
        let skinned = d.palette_slot != NO_PALETTE;
        if let (true, Some(skin), Some(palette_bind)) =
            (skinned, mesh.skin.as_ref(), self.palette.bind.as_ref())
        {
            let Some(pipeline) = self.pipelines.get(&PipelineKey::from_draw(d, true)) else {
                return;
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, camera_bind, camera_off);
            pass.set_bind_group(
                1,
                palette_bind,
                &[(d.palette_slot as u64 * self.palette.stride) as u32],
            );
            pass.set_bind_group(2, textures.texture_bind(d.texture_id), &[]);
            pass.set_bind_group(3, textures.sampler_bind(d.sampler.index()), &[]);
            pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
            pass.set_vertex_buffer(1, skin.slice(..));
            pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(index_range, 0, 0..1);
            return;
        }

        let Some(pipeline) = self.pipelines.get(&PipelineKey::from_draw(d, false)) else {
            return;
        };
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, camera_bind, camera_off);
        pass.set_bind_group(1, world_bind, &[(index as u64 * self.world.stride) as u32]);
        pass.set_bind_group(2, textures.texture_bind(d.texture_id), &[]);
        pass.set_bind_group(3, textures.sampler_bind(d.sampler.index()), &[]);
        pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
        pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(index_range, 0, 0..1);
    }
}
