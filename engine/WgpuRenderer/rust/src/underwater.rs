// Scene-referred underwater compositor. It is run only after the world scene is
// complete and before HDR bloom/exposure/tonemap (or before the LDR UI seam).

use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    time: f32,
    // WGSL aligns the following vec3 to 16 bytes, making the uniform 32 bytes.
    _pad: [f32; 7],
}

const _: () = assert!(std::mem::size_of::<Params>() == 32);

#[test]
fn underwater_wgsl_validates() {
    let module = naga::front::wgsl::parse_str(include_str!("underwater.wgsl"))
        .expect("underwater.wgsl parse");
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .expect("underwater.wgsl validate");
}

pub struct Underwater {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
}

impl Underwater {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_underwater_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("underwater.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_underwater_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_underwater_pipeline_layout"), bind_group_layouts: &[Some(&layout)], immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_underwater_pipeline"), layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: Default::default() },
            primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"), targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }),
            multiview_mask: None, cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_underwater_sampler"), address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge, mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear, ..Default::default()
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_underwater_params"), contents: bytemuck::bytes_of(&Params { time: 0.0, _pad: [0.0; 7] }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self { pipeline, layout, sampler, params }
    }

    pub fn render(&self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder,
                  source: &wgpu::TextureView, depth: &wgpu::TextureView, destination: &wgpu::TextureView, time: f32) {
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&Params { time, _pad: [0.0; 7] }));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_underwater_bind"), layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(source) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(depth) },
                wgpu::BindGroupEntry { binding: 3, resource: self.params.as_entire_binding() },
            ],
        });
        encoder.push_debug_group("wgr_underwater");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wgr_underwater"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: destination, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        encoder.pop_debug_group();
    }
}
