mod textures;

pub use textures::TexFormat;

use glam::Vec2;

use crate::ffi::{WgrBlend, WgrDraw2DBatch, WgrVertex2D};
use textures::{TextureData, TextureRegistry};

pub struct Gfx2d {
    globals_buffer: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipelines: [wgpu::RenderPipeline; 3],

    vbuf: Option<wgpu::Buffer>,
    vbuf_cap: u64,

    white_bind: wgpu::BindGroup,
    #[allow(dead_code)]
    white_tex: wgpu::Texture,

    registry: TextureRegistry,
}

impl Gfx2d {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, surface_format: wgpu::TextureFormat, bc_supported: bool) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_2d_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_2d_globals_layout"),
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

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_2d_texture_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_2d_globals"),
            // 16 bytes: vec2 screen size + vec2 padding.
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_2d_globals_bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buffer.as_entire_binding() }],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_2d_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_2d_pipeline_layout"),
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
            immediate_size: 0,
        });

        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Unorm8x4];
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<WgrVertex2D>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };

        let make_pipeline = |blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("wgr_2d_pipeline"),
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
                depth_stencil: None,
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
        let pipelines = [make_pipeline(None), make_pipeline(Some(alpha)), make_pipeline(Some(additive))];

        let white_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_2d_white"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0xFF, 0xFF, 0xFF, 0xFF],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let white_view = white_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let white_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_2d_white_bind"),
            layout: &texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        Gfx2d {
            globals_buffer,
            globals_bind,
            texture_layout,
            sampler,
            pipelines,
            vbuf: None,
            vbuf_cap: 0,
            white_bind,
            white_tex,
            registry: TextureRegistry::new(bc_supported),
        }
    }

    pub fn texture_create(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        format: TexFormat,
        data: &[u8],
    ) -> u64 {
        let tex = TextureData { width, height, format, bytes: data };
        self.registry.create(device, queue, &self.texture_layout, &self.sampler, &tex)
    }

    pub fn texture_update(&mut self, queue: &wgpu::Queue, handle: u64, data: &[u8]) {
        self.registry.update_rgba(queue, handle, data);
    }

    pub fn texture_destroy(&mut self, handle: u64) {
        self.registry.destroy(handle);
    }

    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, screen: Vec2, verts: &[WgrVertex2D]) {
        let globals = screen.extend(0.0).extend(0.0);
        queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));

        if verts.is_empty() {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(verts);
        let needed = bytes.len() as u64;
        if self.vbuf_cap < needed {
            let cap = needed.next_power_of_two().max(64 * 1024);
            self.vbuf = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_2d_vertices"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.vbuf_cap = cap;
        }
        queue.write_buffer(self.vbuf.as_ref().unwrap(), 0, bytes);
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, batches: &[WgrDraw2DBatch]) {
        let Some(vbuf) = self.vbuf.as_ref() else { return };
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_bind_group(0, &self.globals_bind, &[]);
        for b in batches {
            if b.vertex_count == 0 {
                continue;
            }
            let pipeline = self.pipelines.get(b.blend as usize).unwrap_or(&self.pipelines[WgrBlend::Alpha as usize]);
            let bind = self.registry.get(b.texture_id).map_or(&self.white_bind, |t| &t.bind_group);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(1, bind, &[]);
            pass.draw(b.first_vertex..(b.first_vertex + b.vertex_count), 0..1);
        }
    }
}
