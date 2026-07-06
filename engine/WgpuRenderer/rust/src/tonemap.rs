// The HDR tonemap resolve pass: a fullscreen triangle that samples the offscreen
// HDR scene target and writes the swapchain (exposure -> curve -> optional sRGB
// encode). See docs/hdr-pipeline-plan.md. The pipeline is only constructed when the
// HDR path is enabled; GL33 and the LDR-direct wgpu path never touch it.

use wgpu::util::DeviceExt;

use crate::ffi::WgrTonemap;

pub struct Tonemap {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params_buf: wgpu::Buffer,
    // Rebuilt whenever the HDR source view is (re)created (allocation / resize).
    bind: Option<wgpu::BindGroup>,
}

impl Tonemap {
    pub fn new(device: &wgpu::Device, swapchain_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_tonemap_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tonemap.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_tonemap_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_tonemap_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_tonemap_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: swapchain_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        // 1:1 point sample of the HDR target (same resolution as the swapchain).
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_tonemap_sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_tonemap_params"),
            contents: bytemuck::bytes_of(&WgrTonemap::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Self {
            pipeline,
            layout,
            sampler,
            params_buf,
            bind: None,
        }
    }

    // Point the resolve at a (re)created HDR target view. Called from ensure_hdr.
    pub fn set_source(&mut self, device: &wgpu::Device, hdr_view: &wgpu::TextureView) {
        self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_tonemap_bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(hdr_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params_buf.as_entire_binding(),
                },
            ],
        }));
    }

    pub fn upload_params(&self, queue: &wgpu::Queue, params: &WgrTonemap) {
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(params));
    }

    // Draw the fullscreen resolve into an already-begun render pass targeting the
    // swapchain. No-op until a source view has been set.
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(bind) = self.bind.as_ref() else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
