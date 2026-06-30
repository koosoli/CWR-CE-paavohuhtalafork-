use slotmap::{Key, KeyData, SlotMap};
use wgpu::util::DeviceExt;

use crate::ffi::{WgrDraw3D, WgrMeshVertex};
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

pub struct Gfx3d {
    frame_buf: wgpu::Buffer,
    frame_bind: wgpu::BindGroup,

    world_layout: wgpu::BindGroupLayout,
    // per-draw stride (>= 64, multiple of the uniform offset alignment)
    world_stride: u64,
    world_buf: Option<wgpu::Buffer>,
    world_bind: Option<wgpu::BindGroup>,
    world_cap: u64,

    pipelines: [wgpu::RenderPipeline; 3],

    depth: Option<(wgpu::Texture, wgpu::TextureView)>,
    depth_size: (u32, u32),

    meshes: SlotMap<MeshKey, Mesh>,
}

impl Gfx3d {
    pub fn new(device: &wgpu::Device, textures: &SharedTextures, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_3d_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader3d.wgsl").into()),
        });

        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_3d_frame_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let world_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_3d_world_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(64),
                },
                count: None,
            }],
        });

        let frame_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_3d_frame"),
            // 128 bytes: proj mat4 + view mat4
            size: 128,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_3d_frame_bind"),
            layout: &frame_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: frame_buf.as_entire_binding() }],
        });

        let align = device.limits().min_uniform_buffer_offset_alignment.max(64) as u64;
        let world_stride = 64u64.div_ceil(align) * align;

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_3d_pipeline_layout"),
            bind_group_layouts: &[
                Some(&frame_layout),
                Some(&world_layout),
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
            frame_buf,
            frame_bind,
            world_layout,
            world_stride,
            world_buf: None,
            world_bind: None,
            world_cap: 0,
            pipelines,
            depth: None,
            depth_size: (0, 0),
            meshes: SlotMap::with_key(),
        }
    }

    pub fn mesh_create(&mut self, device: &wgpu::Device, verts: &[WgrMeshVertex], indices: &[u16]) -> u64 {
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
        let key = self.meshes.insert(Mesh { vbuf, ibuf, index_count: indices.len() as u32 });
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
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
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

    // Upload the frame matrices and every draw's world matrix; (re)grow the dynamic world UBO as needed
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, proj: &[f32; 16], view: &[f32; 16],
                   draws: &[WgrDraw3D]) {
        let mut frame = [0.0f32; 32];
        frame[..16].copy_from_slice(proj);
        frame[16..].copy_from_slice(view);
        queue.write_buffer(&self.frame_buf, 0, bytemuck::cast_slice(&frame));

        if draws.is_empty() {
            return;
        }
        let needed = draws.len() as u64 * self.world_stride;
        if self.world_cap < needed {
            let cap = needed.next_power_of_two().max(self.world_stride * 64);
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_3d_world"),
                size: cap,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.world_bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wgr_3d_world_bind"),
                layout: &self.world_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(64),
                    }),
                }],
            }));
            self.world_buf = Some(buf);
            self.world_cap = cap;
        }
        let world_buf = self.world_buf.as_ref().unwrap();
        for (i, d) in draws.iter().enumerate() {
            queue.write_buffer(world_buf, i as u64 * self.world_stride, bytemuck::bytes_of(&d.world));
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, textures: &SharedTextures, draws: &[WgrDraw3D]) {
        let Some(world_bind) = self.world_bind.as_ref() else { return };
        pass.set_bind_group(0, &self.frame_bind, &[]);
        for (i, d) in draws.iter().enumerate() {
            if d.index_count == 0 {
                continue;
            }
            let Some(mesh) = self.meshes.get(KeyData::from_ffi(d.mesh).into()) else { continue };
            // Never issue an indexed draw past the mesh's index buffer
            if d.index_begin + d.index_count > mesh.index_count {
                continue;
            }
            let pipeline = self.pipelines.get(d.blend as usize).unwrap_or(&self.pipelines[0]);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, world_bind, &[(i as u64 * self.world_stride) as u32]);
            pass.set_bind_group(2, textures.texture_bind(d.texture_id), &[]);
            pass.set_bind_group(3, textures.sampler_bind(d.sampler.index()), &[]);
            pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
            pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(d.index_begin..(d.index_begin + d.index_count), 0, 0..1);
        }
    }
}
