// Scene-referred underwater compositor plus its frustum-aligned light volume and
// FFT-derived caustic field. The compute work is dispatched only while the camera
// is in the displaced-waterline activation band.

use bytemuck::Zeroable;
use wgpu::util::DeviceExt;

const FROXEL_W: u32 = 32;
const FROXEL_H: u32 = 18;
const FROXEL_D: u32 = 32;
const CAUSTIC_SIZE: u32 = 128;
const VOLUME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    // x = time, y = camera height over the local surface, z = volume range, w = extinction.
    time_height_range_ext: [f32; 4],
    // xyz = absolute camera position, w = active FFT cascade count.
    camera_pos_layers: [f32; 4],
    // xyz = direction from the surface to the sun, w = water debug-view selector.
    sun_dir_debug: [f32; 4],
    // xyz = scene-linear direct sunlight, w unused.
    sun_radiance: [f32; 4],
    // inv(view) * inv(proj), matching Frame.inv_view_proj.
    inv_view_proj: [f32; 16],
    // Authored water colours are gamma-space.
    shallow_color: [f32; 4],
    deep_color: [f32; 4],
    cascade_lengths: [f32; 4],
    // x = de-tile warp amplitude, y = sea level, z/w reserved.
    water_controls: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<Params>() == 192);

#[test]
fn underwater_wgsl_validates() {
    for (name, source) in [
        ("underwater.wgsl", include_str!("underwater.wgsl")),
        (
            "underwater_froxel.wgsl",
            include_str!("underwater_froxel.wgsl"),
        ),
        (
            "underwater_caustics.wgsl",
            include_str!("underwater_caustics.wgsl"),
        ),
    ] {
        let module = naga::front::wgsl::parse_str(source).unwrap_or_else(|e| panic!("{name}: {e}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn underwater_refraction_rejects_foreground_depth_leaks() {
    let shader = include_str!("underwater.wgsl");
    assert!(shader.contains("let warp_limit = 3.0 / dims_f"));
    assert!(shader.contains("let use_warp = warped_depth <= base_depth + 0.001"));
    assert!(shader.contains("fn water_path_length"));
    assert!(shader.contains("let extinction_rgb = vec3<f32>(0.280, 0.065, 0.020)"));
    assert!(shader.contains("underwater_froxel"));
    assert!(!shader.contains("pow(deep_linear"));
}

#[test]
fn underwater_volume_is_shadowed_and_caustics_follow_fft() {
    let froxel = include_str!("underwater_froxel.wgsl");
    assert!(froxel.contains("terrain_occlusion"));
    assert!(froxel.contains("csm_occlusion"));
    assert!(froxel.contains("froxel_out"));
    let caustics = include_str!("underwater_caustics.wgsl");
    assert!(caustics.contains("fft_dynamics"));
    assert!(caustics.contains("fft_auxiliary"));
    assert!(caustics.contains("fft_aperiodic_uv"));
}

#[test]
fn default_absorption_keeps_useful_midrange_visibility() {
    let extinction_scale = (0.16_f32 * 2.5).max(0.12);
    let transmission = |sigma: f32, distance: f32| (-sigma * extinction_scale * distance).exp();

    assert!(transmission(0.280, 10.0) > 0.30);
    assert!(transmission(0.065, 10.0) > 0.75);
    assert!(transmission(0.020, 30.0) > 0.75);
}

struct Volume {
    froxel_pipeline: wgpu::ComputePipeline,
    froxel_layout: wgpu::BindGroupLayout,
    caustic_pipeline: wgpu::ComputePipeline,
    caustic_layout: wgpu::BindGroupLayout,
    shadow_sampler: wgpu::Sampler,
    csm_sampler: wgpu::Sampler,
    fft_sampler: wgpu::Sampler,
    shadow_mapping: wgpu::Buffer,
    camera_shadow: wgpu::Buffer,
}

pub struct Underwater {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    _froxel_tex: wgpu::Texture,
    froxel_view: wgpu::TextureView,
    _caustic_tex: wgpu::Texture,
    caustic_view: wgpu::TextureView,
    volume: Option<Volume>,
}

impl Underwater {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        volume_storage_supported: bool,
    ) -> Self {
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
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_underwater_params"),
            contents: bytemuck::bytes_of(&Params::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let volume_usage = wgpu::TextureUsages::TEXTURE_BINDING
            | if volume_storage_supported {
                wgpu::TextureUsages::STORAGE_BINDING
            } else {
                wgpu::TextureUsages::empty()
            };
        let froxel_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_underwater_froxel"),
            size: wgpu::Extent3d {
                width: FROXEL_W,
                height: FROXEL_H,
                depth_or_array_layers: FROXEL_D,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: VOLUME_FORMAT,
            usage: volume_usage,
            view_formats: &[],
        });
        let froxel_view = froxel_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("wgr_underwater_froxel_view"),
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });
        let caustic_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_underwater_caustics"),
            size: wgpu::Extent3d {
                width: CAUSTIC_SIZE,
                height: CAUSTIC_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: VOLUME_FORMAT,
            usage: volume_usage,
            view_formats: &[],
        });
        let caustic_view = caustic_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let volume = volume_storage_supported.then(|| {
            let froxel_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("wgr_underwater_froxel_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("underwater_froxel.wgsl").into()),
            });
            let uniform = |binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            };
            let froxel_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("wgr_underwater_froxel_layout"),
                entries: &[
                    uniform(0),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: VOLUME_FORMAT,
                            view_dimension: wgpu::TextureViewDimension::D3,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    uniform(4),
                    wgpu::BindGroupLayoutEntry {
                        binding: 5,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                    uniform(7),
                ],
            });
            let froxel_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wgr_underwater_froxel_pipeline_layout"),
                bind_group_layouts: &[Some(&froxel_layout)],
                immediate_size: 0,
            });
            let froxel_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("wgr_underwater_froxel_pipeline"),
                    layout: Some(&froxel_pl),
                    module: &froxel_shader,
                    entry_point: Some("cs_underwater_froxel"),
                    compilation_options: Default::default(),
                    cache: None,
                });

            let caustic_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("wgr_underwater_caustic_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("underwater_caustics.wgsl").into()),
            });
            let caustic_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("wgr_underwater_caustic_layout"),
                    entries: &[
                        uniform(0),
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2Array,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 4,
                            visibility: wgpu::ShaderStages::COMPUTE,
                            ty: wgpu::BindingType::StorageTexture {
                                access: wgpu::StorageTextureAccess::WriteOnly,
                                format: VOLUME_FORMAT,
                                view_dimension: wgpu::TextureViewDimension::D2,
                            },
                            count: None,
                        },
                    ],
                });
            let caustic_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wgr_underwater_caustic_pipeline_layout"),
                bind_group_layouts: &[Some(&caustic_layout)],
                immediate_size: 0,
            });
            let caustic_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("wgr_underwater_caustic_pipeline"),
                    layout: Some(&caustic_pl),
                    module: &caustic_shader,
                    entry_point: Some("cs_underwater_caustics"),
                    compilation_options: Default::default(),
                    cache: None,
                });
            let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("wgr_underwater_shadow_sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let csm_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("wgr_underwater_csm_sampler"),
                compare: Some(wgpu::CompareFunction::LessEqual),
                ..Default::default()
            });
            let fft_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("wgr_underwater_fft_sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            });
            let shadow_mapping = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("wgr_underwater_shadow_mapping"),
                contents: bytemuck::bytes_of(&crate::terrain::TerrainShadowMap::zeroed()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let camera_shadow = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("wgr_underwater_camera_shadow"),
                contents: bytemuck::bytes_of(&crate::ffi::WgrCameraShadow::zeroed()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            Volume {
                froxel_pipeline,
                froxel_layout,
                caustic_pipeline,
                caustic_layout,
                shadow_sampler,
                csm_sampler,
                fft_sampler,
                shadow_mapping,
                camera_shadow,
            }
        });

        Self {
            pipeline,
            layout,
            sampler,
            params,
            _froxel_tex: froxel_tex,
            froxel_view,
            _caustic_tex: caustic_tex,
            caustic_view,
            volume,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn upload(
        &self,
        queue: &wgpu::Queue,
        time: f32,
        cam_above: f32,
        camera_pos: [f32; 3],
        inv_view_proj: [f32; 16],
        shallow_color: [f32; 4],
        deep_color: [f32; 4],
        sun_dir: [f32; 3],
        sun_radiance: [f32; 3],
        cascade_lengths: [f32; 4],
        active_layers: u32,
        warp_amp: f32,
        sea_level: f32,
        debug_view: f32,
        shadow_mapping: &crate::terrain::TerrainShadowMap,
        camera_shadow: &crate::ffi::WgrCameraShadow,
    ) {
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                time_height_range_ext: [time, cam_above, 120.0, shallow_color[3]],
                camera_pos_layers: [
                    camera_pos[0],
                    camera_pos[1],
                    camera_pos[2],
                    active_layers as f32,
                ],
                sun_dir_debug: [sun_dir[0], sun_dir[1], sun_dir[2], debug_view],
                sun_radiance: [sun_radiance[0], sun_radiance[1], sun_radiance[2], 0.0],
                inv_view_proj,
                shallow_color,
                deep_color,
                cascade_lengths,
                water_controls: [warp_amp, sea_level, 0.0, 0.0],
            }),
        );
        if let Some(volume) = &self.volume {
            queue.write_buffer(
                &volume.shadow_mapping,
                0,
                bytemuck::bytes_of(shadow_mapping),
            );
            queue.write_buffer(&volume.camera_shadow, 0, bytemuck::bytes_of(camera_shadow));
        }
    }

    pub fn render_froxel(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        shadow_mask: &wgpu::TextureView,
        csm_view: &wgpu::TextureView,
    ) {
        let Some(volume) = &self.volume else {
            return;
        };
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_underwater_froxel_bind"),
            layout: &volume.froxel_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.froxel_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(shadow_mask),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&volume.shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: volume.shadow_mapping.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(csm_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&volume.csm_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: volume.camera_shadow.as_entire_binding(),
                },
            ],
        });
        encoder.push_debug_group("wgr_underwater_froxel");
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_underwater_froxel"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&volume.froxel_pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(FROXEL_W.div_ceil(8), FROXEL_H.div_ceil(8), 1);
        drop(pass);
        encoder.pop_debug_group();
    }

    pub fn render_caustics(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        fft_dynamics: &wgpu::TextureView,
        fft_auxiliary: &wgpu::TextureView,
    ) {
        let Some(volume) = &self.volume else {
            return;
        };
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_underwater_caustic_bind"),
            layout: &volume.caustic_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(fft_dynamics),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(fft_auxiliary),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&volume.fft_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&self.caustic_view),
                },
            ],
        });
        encoder.push_debug_group("wgr_underwater_caustics");
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_underwater_caustics"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&volume.caustic_pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(CAUSTIC_SIZE.div_ceil(8), CAUSTIC_SIZE.div_ceil(8), 1);
        drop(pass);
        encoder.pop_debug_group();
    }

    pub fn render(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        destination: &wgpu::TextureView,
    ) {
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
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&self.froxel_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&self.caustic_view),
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
