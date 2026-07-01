use glam::Vec2;

use crate::ffi::{WgrBlend, WgrDraw2DBatch, WgrVertex2D};
use crate::gfx3d::DEPTH_FORMAT;
use crate::textures::SharedTextures;

pub struct Gfx2d {
    globals_buffer: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    // [depth_mode][blend]: depth mode = WgrDepthMode (none / test / test+write).
    pipelines: [[wgpu::RenderPipeline; 3]; 3],

    vbuf: Option<wgpu::Buffer>,
    vbuf_cap: u64,
}

impl Gfx2d {
    pub fn new(
        device: &wgpu::Device,
        textures: &SharedTextures,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_2d_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_2d_globals_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_2d_globals"),
            // 32 bytes: vec2 screen size + vec2 padding + vec4 fog colour.
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_2d_globals_bind"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_2d_pipeline_layout"),
            bind_group_layouts: &[
                Some(&globals_layout),
                Some(&textures.texture_layout),
                Some(&textures.sampler_layout),
            ],
            immediate_size: 0,
        });

        // pos(x,y,z), (rhw, fog), uv, color.
        let attrs =
            wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x2, 3 => Unorm8x4];
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<WgrVertex2D>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };

        // (test, write): plain 2D / sky use (false,false); transparent meshes
        // (false… ) — see callers below. test gates LessEqual vs Always.
        let make_pipeline = |blend: Option<wgpu::BlendState>, test: bool, write: bool| {
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
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(write),
                    depth_compare: Some(if test {
                        wgpu::CompareFunction::LessEqual
                    } else {
                        wgpu::CompareFunction::Always
                    }),
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
        // Indexed by WgrDepthMode: 0 none, 1 test (no write), 2 test+write.
        let blends = [None, Some(alpha), Some(additive)];
        let pipelines = [
            std::array::from_fn(|b| make_pipeline(blends[b], false, false)),
            std::array::from_fn(|b| make_pipeline(blends[b], true, false)),
            std::array::from_fn(|b| make_pipeline(blends[b], true, true)),
        ];

        Gfx2d {
            globals_buffer,
            globals_bind,
            pipelines,
            vbuf: None,
            vbuf_cap: 0,
        }
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: Vec2,
        fog: [f32; 3],
        verts: &[WgrVertex2D],
    ) {
        // 8 floats: screen.xy, pad.xy, fog.rgb, pad.
        let globals = [screen.x, screen.y, 0.0, 0.0, fog[0], fog[1], fog[2], 0.0];
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

    // Draw one batch. Re-binds the vertex buffer + globals every call because 3D
    // draws interleaved in the same pass clobber vertex buffer slot 0. The batch's
    // depth mode picks the pipeline set (plain 2D = none; pre-projected meshes test
    // and, when opaque, write).
    pub fn draw_one(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        textures: &SharedTextures,
        b: &WgrDraw2DBatch,
    ) {
        if b.vertex_count == 0 {
            return;
        }
        let Some(vbuf) = self.vbuf.as_ref() else {
            return;
        };
        let set = self
            .pipelines
            .get(b.depth as usize)
            .unwrap_or(&self.pipelines[0]);
        let pipeline = set
            .get(b.blend as usize)
            .unwrap_or(&set[WgrBlend::Alpha as usize]);
        pass.set_pipeline(pipeline);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_bind_group(0, &self.globals_bind, &[]);
        pass.set_bind_group(1, textures.texture_bind(b.texture_id), &[]);
        pass.set_bind_group(2, textures.sampler_bind(b.sampler.index()), &[]);
        pass.draw(b.first_vertex..(b.first_vertex + b.vertex_count), 0..1);
    }
}
