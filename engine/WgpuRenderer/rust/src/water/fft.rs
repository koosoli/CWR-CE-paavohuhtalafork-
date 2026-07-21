/// Shared ocean resolution. 256² resolves the medium/small wind bands noticeably better
/// than 128² while remaining practical for the four-cascade compute path.
pub const FFT_RESOLUTION: u32 = 256;
const FFT_LAYERS: u32 = 4;
const FFT_STAGES: u32 = 8;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StageParams {
    data: [u32; 4],
}

pub struct Fft {
    displacement: wgpu::TextureView,
    dynamics: wgpu::TextureView,
    auxiliary: wgpu::TextureView,
    spectrum_bind: wgpu::BindGroup,
    stage_binds: Vec<wgpu::BindGroup>,
    compose_bind: wgpu::BindGroup,
    spectrum_pipeline: wgpu::ComputePipeline,
    stage_pipeline: wgpu::ComputePipeline,
    compose_pipeline: wgpu::ComputePipeline,
    stage_alignment: u32,
}

impl Fft {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        composer: &mut naga_oil::compose::Composer,
        water_params: &wgpu::Buffer,
        storage_supported: bool,
    ) -> Option<Self> {
        if !storage_supported || std::env::var("WGR_WATER_FFT").ok().as_deref() == Some("0") {
            return None;
        }
        let array_view = |format, usage, label: &str| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: FFT_RESOLUTION,
                        height: FFT_RESOLUTION,
                        depth_or_array_layers: FFT_LAYERS,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2Array),
                    ..Default::default()
                })
        };
        let packs = [
            [
                array_view(
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    "wgr_fft_pack0_a",
                ),
                array_view(
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    "wgr_fft_pack0_b",
                ),
            ],
            [
                array_view(
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    "wgr_fft_pack1_a",
                ),
                array_view(
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    "wgr_fft_pack1_b",
                ),
            ],
            [
                array_view(
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    "wgr_fft_pack2_a",
                ),
                array_view(
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    "wgr_fft_pack2_b",
                ),
            ],
        ];
        let out_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING;
        let displacement = array_view(
            wgpu::TextureFormat::Rgba16Float,
            out_usage,
            "wgr_fft_displacement",
        );
        let dynamics = array_view(
            wgpu::TextureFormat::Rgba16Float,
            out_usage,
            "wgr_fft_dynamics",
        );
        let auxiliary = array_view(
            wgpu::TextureFormat::Rgba16Float,
            out_usage,
            "wgr_fft_auxiliary",
        );
        let uniform_layout = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let spectrum_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_fft_spectrum_layout"),
            entries: &[
                uniform_layout(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                uniform_layout(
                    1,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
                uniform_layout(
                    2,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
                uniform_layout(
                    3,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
            ],
        });
        let spectrum_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_fft_spectrum_bind"),
            layout: &spectrum_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: water_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&packs[0][0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&packs[1][0]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&packs[2][0]),
                },
            ],
        });
        let stage_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_fft_stage_layout"),
            entries: &[
                uniform_layout(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: None,
                    },
                ),
                uniform_layout(
                    1,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                uniform_layout(
                    2,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
            ],
        });
        // Each pass has one source/destination pair per complex pack. Dynamic offsets select its stage.
        let mut stage_binds = Vec::new();
        let aligned = device.limits().min_uniform_buffer_offset_alignment as u64;
        let stage_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_fft_stage_params"),
            size: aligned * (FFT_STAGES as u64 * 2),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        for axis in 0..2 {
            for stage in 0..FFT_STAGES {
                for pack in 0..3 {
                    let parity = ((axis * FFT_STAGES + stage) & 1) as usize;
                    stage_binds.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("wgr_fft_stage_bind"),
                        layout: &stage_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer: &stage_buffer,
                                    offset: 0,
                                    size: std::num::NonZeroU64::new(16),
                                }),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&packs[pack][parity]),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(
                                    &packs[pack][1 - parity],
                                ),
                            },
                        ],
                    }));
                }
            }
        }
        // Parameters are immutable structural values, uploaded once to the dynamically addressed UBO.
        let mut bytes = vec![0u8; (aligned * (FFT_STAGES as u64 * 2)) as usize];
        for axis in 0..2 {
            for stage in 0..FFT_STAGES {
                let offset = ((axis * FFT_STAGES + stage) as u64 * aligned) as usize;
                bytes[offset..offset + 16].copy_from_slice(bytemuck::bytes_of(&StageParams {
                    data: [
                        FFT_RESOLUTION,
                        stage,
                        axis,
                        if axis == 1 && stage + 1 == FFT_STAGES {
                            1
                        } else {
                            0
                        },
                    ],
                }));
            }
        }
        queue.write_buffer(&stage_buffer, 0, &bytes);
        let compose_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_fft_compose_layout"),
            entries: &[
                uniform_layout(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                uniform_layout(
                    1,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                uniform_layout(
                    2,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                uniform_layout(
                    3,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                uniform_layout(
                    4,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
                uniform_layout(
                    5,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
                uniform_layout(
                    6,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
            ],
        });
        // X starts in ping (0) and ends in pong; Y starts from that pong and ends in ping.
        let final_parity = 0usize;
        let compose_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_fft_compose_bind"),
            layout: &compose_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: water_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&packs[0][final_parity]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&packs[1][final_parity]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&packs[2][final_parity]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&displacement),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&dynamics),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&auxiliary),
                },
            ],
        });
        let mut pipeline = |label, source, entry, layout: &wgpu::BindGroupLayout| {
            let shader = crate::shaders::make_module(device, composer, label, source, label);
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(
                    &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some(label),
                        bind_group_layouts: &[Some(layout)],
                        immediate_size: 0,
                    }),
                ),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Some(Self {
            displacement,
            dynamics,
            auxiliary,
            spectrum_bind,
            stage_binds,
            compose_bind,
            spectrum_pipeline: pipeline(
                "water/fft_spectrum.wgsl",
                include_str!("fft_spectrum.wgsl"),
                "fft_spectrum_evolve",
                &spectrum_layout,
            ),
            stage_pipeline: pipeline(
                "water/fft_stage.wgsl",
                include_str!("fft_stage.wgsl"),
                "fft_stage",
                &stage_layout,
            ),
            compose_pipeline: pipeline(
                "water/fft_compose.wgsl",
                include_str!("fft_compose.wgsl"),
                "fft_compose",
                &compose_layout,
            ),
            stage_alignment: aligned as u32,
        })
    }
    pub fn displacement_view(&self) -> &wgpu::TextureView {
        &self.displacement
    }
    pub fn dynamics_view(&self) -> &wgpu::TextureView {
        &self.dynamics
    }
    pub fn auxiliary_view(&self) -> &wgpu::TextureView {
        &self.auxiliary
    }
    pub fn dispatch(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_water_fft"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.spectrum_pipeline);
        pass.set_bind_group(0, &self.spectrum_bind, &[]);
        pass.dispatch_workgroups(FFT_RESOLUTION / 8, FFT_RESOLUTION / 8, FFT_LAYERS);
        pass.set_pipeline(&self.stage_pipeline);
        for axis in 0..2 {
            for stage in 0..FFT_STAGES {
                for pack in 0..3 {
                    let index = ((axis * FFT_STAGES + stage) * 3 + pack) as usize;
                    pass.set_bind_group(
                        0,
                        &self.stage_binds[index],
                        &[(axis * FFT_STAGES + stage) * self.stage_alignment],
                    );
                    pass.dispatch_workgroups(FFT_RESOLUTION / 8, FFT_RESOLUTION / 8, FFT_LAYERS);
                }
            }
        }
        pass.set_pipeline(&self.compose_pipeline);
        pass.set_bind_group(0, &self.compose_bind, &[]);
        pass.dispatch_workgroups(FFT_RESOLUTION / 8, FFT_RESOLUTION / 8, FFT_LAYERS);
    }
}
