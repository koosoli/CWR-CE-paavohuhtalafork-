use slotmap::{Key, KeyData, SlotMap};
use wgpu::util::DeviceExt;

use crate::ffi::{WgrCamera, WgrDraw3D, WgrMeshVertex};
use crate::textures::SharedTextures;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

slotmap::new_key_type! {
    struct MeshKey;
}

struct Mesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
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
    fn new(device: &wgpu::Device, label: &str, bind_size: u64, visibility: wgpu::ShaderStages) -> Self {
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
    cameras: DynUbo,
    world: DynUbo,

    pipelines: [wgpu::RenderPipeline; 3],

    depth: Option<(wgpu::Texture, wgpu::TextureView)>,
    depth_size: (u32, u32),

    meshes: SlotMap<MeshKey, Mesh>,
}

impl Gfx3d {
    pub fn new(
        device: &wgpu::Device,
        textures: &SharedTextures,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_3d_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader3d.wgsl").into()),
        });

        // Both UBOs are dynamic-offset: the camera buffer holds one entry per
        // distinct view/proj this frame, the world buffer one per draw.
        // Camera UBO is read by both stages (vertex: proj/view + fog factor;
        // fragment: fog_color). World UBO is vertex-only.
        let cameras = DynUbo::new(
            device,
            "wgr_3d_camera_layout",
            std::mem::size_of::<WgrCamera>() as u64,
            wgpu::ShaderStages::VERTEX_FRAGMENT,
        );
        let world = DynUbo::new(device, "wgr_3d_world_layout", 64, wgpu::ShaderStages::VERTEX);

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

        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<WgrMeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };

        let make_pipeline = |blend: Option<wgpu::BlendState>, depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("wgr_3d_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: std::slice::from_ref(&vbuf_layout),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let alpha = wgpu::BlendState {
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
        };
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };
        let pipelines = [
            make_pipeline(None, true),
            make_pipeline(Some(alpha), false),
            make_pipeline(Some(additive), false),
        ];

        Gfx3d {
            cameras,
            world,
            pipelines,
            depth: None,
            depth_size: (0, 0),
            meshes: SlotMap::with_key(),
        }
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
            usage: wgpu::BufferUsages::VERTEX,
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
        });
        key.data().as_ffi()
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

    // Upload every camera and every draw's world matrix; regrow the dynamic UBOs.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cameras: &[WgrCamera],
        draws: &[WgrDraw3D],
    ) {
        if !cameras.is_empty() {
            self.cameras.ensure(device, cameras.len());
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
        let pipeline = self
            .pipelines
            .get(d.blend as usize)
            .unwrap_or(&self.pipelines[0]);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(
            0,
            camera_bind,
            &[(d.camera as u64 * self.cameras.stride) as u32],
        );
        pass.set_bind_group(1, world_bind, &[(index as u64 * self.world.stride) as u32]);
        pass.set_bind_group(2, textures.texture_bind(d.texture_id), &[]);
        pass.set_bind_group(3, textures.sampler_bind(d.sampler.index()), &[]);
        pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
        pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(d.index_begin..(d.index_begin + d.index_count), 0, 0..1);
    }
}
