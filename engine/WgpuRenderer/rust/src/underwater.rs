// Scene-referred underwater compositor. It is run only after the world scene is
// complete and before HDR bloom/exposure/tonemap (or before the LDR UI seam).

use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    time: f32,
    // Camera height above the local water surface in metres; negative means the eye is submerged.
    // The fragment shader intersects the view ray with that surface for a per-pixel water path.
    cam_above: f32,
    // Absolute camera X/Z makes the seabed caustic pattern stable under translation.
    camera_xz: [f32; 2],
    // inv(view) * inv(proj), matching Frame.inv_view_proj: unprojects forward-NDC to a
    // camera-relative world position.
    inv_view_proj: [f32; 16],
    // Authored colours are gamma-space. Extinction is packed in shallow_color_ext.w.
    shallow_color_ext: [f32; 4],
    deep_color: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<Params>() == 112);

#[test]
fn underwater_wgsl_validates() {
    let module = naga::front::wgsl::parse_str(include_str!("underwater.wgsl"))
        .expect("underwater.wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("underwater.wgsl validate");
}

#[test]
fn underwater_refraction_rejects_foreground_depth_leaks() {
    let shader = include_str!("underwater.wgsl");
    assert!(shader.contains("let warp_limit = 3.0 / dims_f"));
    assert!(shader.contains("let use_warp = warped_depth <= base_depth + 0.001"));
    assert!(shader.contains("fn water_path_length"));
    assert!(shader.contains("let extinction_rgb = vec3<f32>(0.280, 0.065, 0.020)"));
    assert!(!shader.contains("pow(deep_linear"));
}

#[test]
fn default_absorption_keeps_useful_midrange_visibility() {
    let extinction_scale = (0.16_f32 * 2.5).max(0.12);
    let transmission =
        |sigma: f32, distance: f32| (-sigma * extinction_scale * distance).exp();

    assert!(transmission(0.280, 10.0) > 0.30);
    assert!(transmission(0.065, 10.0) > 0.75);
    assert!(transmission(0.020, 30.0) > 0.75);
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
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
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
            label: Some("wgr_underwater_pipeline_layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_underwater_pipeline"),
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
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_underwater_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_underwater_params"),
            contents: bytemuck::bytes_of(&Params {
                time: 0.0,
                cam_above: -1.0,
                camera_xz: [0.0; 2],
                inv_view_proj: [0.0; 16],
                shallow_color_ext: [0.070, 0.290, 0.320, 0.16],
                deep_color: [0.014, 0.105, 0.240, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            pipeline,
            layout,
            sampler,
            params,
        }
    }

    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        destination: &wgpu::TextureView,
        time: f32,
        cam_above: f32,
        camera_xz: [f32; 2],
        inv_view_proj: [f32; 16],
        shallow_color_ext: [f32; 4],
        deep_color: [f32; 4],
    ) {
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                time,
                cam_above,
                camera_xz,
                inv_view_proj,
                shallow_color_ext,
                deep_color,
            }),
        );
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_underwater_bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(depth),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        encoder.push_debug_group("wgr_underwater");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wgr_underwater"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        encoder.pop_debug_group();
    }
}
