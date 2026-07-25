/// Shared ocean resolution. 256² resolves the medium/small wind bands noticeably better
/// than 128² while remaining practical for the four-cascade compute path.
pub const FFT_RESOLUTION: u32 = 256;
const FFT_LAYERS: u32 = 4;
const FFT_STAGES: u32 = 8;

// These are the only WaterParams values used to construct h0. Compare their raw bits so live
// per-frame uploads do not recreate the spectrum, while every authored spectrum change does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpectrumInputs {
    enabled: u32,
    seed: u32,
    wave_amp: u32,
    wind_sea: [u32; 4],
    cascade_lengths: [u32; 4],
}

impl SpectrumInputs {
    fn from_params(params: &crate::ffi::WgrWaterParams) -> Self {
        Self {
            enabled: params.fft_control[0].to_bits(),
            seed: params.fft_control[1].to_bits(),
            wave_amp: params.wave_amp.to_bits(),
            wind_sea: params.fft_wind_sea.map(f32::to_bits),
            cascade_lengths: params.fft_cascade_lengths.map(f32::to_bits),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StageParams {
    data: [u32; 4],
}

pub struct Fft {
    displacement: wgpu::TextureView,
    dynamics: wgpu::TextureView,
    auxiliary: wgpu::TextureView,
    h0_init_bind: wgpu::BindGroup,
    spectrum_bind: wgpu::BindGroup,
    stage_binds: Vec<wgpu::BindGroup>,
    compose_bind: wgpu::BindGroup,
    h0_init_pipeline: wgpu::ComputePipeline,
    spectrum_pipeline: wgpu::ComputePipeline,
    stage_pipeline: wgpu::ComputePipeline,
    compose_pipeline: wgpu::ComputePipeline,
    stage_alignment: u32,
    spectrum_inputs: Option<SpectrumInputs>,
    spectrum_dirty: bool,
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
        // h0 is the persistent random field. It is written only after spectrum inputs change and
        // read by the per-frame evolution pass.
        let h0 = array_view(
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            "wgr_fft_h0",
        );
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
        let h0_init_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_fft_h0_init_layout"),
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
            ],
        });
        let h0_init_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_fft_h0_init_bind"),
            layout: &h0_init_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: water_params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&h0),
                },
            ],
        });
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
                uniform_layout(
                    3,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                    },
                ),
                uniform_layout(
                    4,
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
                    resource: wgpu::BindingResource::TextureView(&h0),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&packs[0][0]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&packs[1][0]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
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
            h0_init_bind,
            spectrum_bind,
            stage_binds,
            compose_bind,
            h0_init_pipeline: pipeline(
                "water/fft_spectrum_init.wgsl",
                include_str!("fft_spectrum_init.wgsl"),
                "fft_spectrum_init",
                &h0_init_layout,
            ),
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
            spectrum_inputs: None,
            spectrum_dirty: true,
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
    pub fn set_params(&mut self, params: &crate::ffi::WgrWaterParams) {
        let inputs = SpectrumInputs::from_params(params);
        if self.spectrum_inputs != Some(inputs) {
            self.spectrum_inputs = Some(inputs);
            self.spectrum_dirty = true;
        }
    }
    pub fn set_cascade_config(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _index: u32,
        _config: crate::ffi::WgrWaterCascadeConfig,
    ) {
        self.spectrum_dirty = true;
    }
    pub fn dispatch(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        timers: &crate::gpu_timers::GpuTimers,
    ) {
        use crate::gpu_timers::Region;
        // WTR-002 — each FFT phase runs in its own compute pass so the encoder-level
        // timestamps bracket exactly one phase (spectrum init / evolve / horizontal /
        // vertical / compose). Pass splitting adds no barriers beyond those wgpu already
        // inserts between the dependent storage writes, so the GPU work is unchanged.
        fn compute<'e>(
            encoder: &'e mut wgpu::CommandEncoder,
            label: &str,
        ) -> wgpu::ComputePass<'e> {
            encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: None,
            })
        }
        if self.spectrum_dirty {
            timers.begin(encoder, Region::SpectrumInit);
            let mut pass = compute(encoder, "wgr_water_fft_spectrum_init");
            pass.set_pipeline(&self.h0_init_pipeline);
            pass.set_bind_group(0, &self.h0_init_bind, &[]);
            pass.dispatch_workgroups(FFT_RESOLUTION / 8, FFT_RESOLUTION / 8, FFT_LAYERS);
            drop(pass);
            timers.end(encoder, Region::SpectrumInit);
            self.spectrum_dirty = false;
        }
        timers.begin(encoder, Region::SpectrumEvolve);
        {
            let mut pass = compute(encoder, "wgr_water_fft_spectrum_evolve");
            pass.set_pipeline(&self.spectrum_pipeline);
            pass.set_bind_group(0, &self.spectrum_bind, &[]);
            pass.dispatch_workgroups(FFT_RESOLUTION / 8, FFT_RESOLUTION / 8, FFT_LAYERS);
        }
        timers.end(encoder, Region::SpectrumEvolve);
        for axis in 0..2 {
            let (region, label) = if axis == 0 {
                (Region::FftHorizontal, "wgr_water_fft_horizontal")
            } else {
                (Region::FftVertical, "wgr_water_fft_vertical")
            };
            timers.begin(encoder, region);
            let mut pass = compute(encoder, label);
            pass.set_pipeline(&self.stage_pipeline);
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
            drop(pass);
            timers.end(encoder, region);
        }
        timers.begin(encoder, Region::FftCompose);
        {
            let mut pass = compute(encoder, "wgr_water_fft_compose");
            pass.set_pipeline(&self.compose_pipeline);
            pass.set_bind_group(0, &self.compose_bind, &[]);
            pass.dispatch_workgroups(FFT_RESOLUTION / 8, FFT_RESOLUTION / 8, FFT_LAYERS);
        }
        timers.end(encoder, Region::FftCompose);
    }
}

#[cfg(test)]
mod tests {
    use super::SpectrumInputs;
    use bytemuck::Zeroable;

    const TAU: f32 = std::f32::consts::TAU;

    fn hash(value: u32) -> u32 {
        let mut x = value;
        x = (x ^ (x >> 16)).wrapping_mul(0x7feb_352d);
        x = (x ^ (x >> 15)).wrapping_mul(0x846c_a68b);
        x ^ (x >> 16)
    }

    fn cascade_transition(k: f32, split: f32) -> f32 {
        let edge0 = split * 0.72;
        let edge1 = split * 1.38;
        let t = ((k - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    fn cascade_bands(k: f32, lengths: [f32; 4]) -> [f32; 4] {
        let split =
            |lower: usize, upper: usize| (TAU / lengths[lower] * TAU / lengths[upper]).sqrt();
        let low_to_mid_low = cascade_transition(k, split(2, 3));
        let mid_low_to_mid_high = cascade_transition(k, split(1, 2));
        let mid_high_to_high = cascade_transition(k, split(0, 1));
        [
            mid_high_to_high,
            mid_low_to_mid_high * (1.0 - mid_high_to_high),
            low_to_mid_low * (1.0 - mid_low_to_mid_high),
            1.0 - low_to_mid_low,
        ]
    }

    fn swell_angle(seed: u32) -> f32 {
        let random = (hash(seed ^ 0x68bc_21eb) & 0x00ff_ffff) as f32 / 16_777_216.0;
        let sign = if hash(seed ^ 0x02e5_be93) & 1 == 0 {
            1.0
        } else {
            -1.0
        };
        sign * (0.45 + (0.95 - 0.45) * random)
    }

    fn jonswap_k_shape(k: f32, peak_k: f32, alpha: f32, gamma: f32) -> f32 {
        let ratio = (k / peak_k).max(1e-4);
        let sigma = if ratio <= 1.0 { 0.07 } else { 0.09 };
        let peak = (-0.5 * ((ratio - 1.0) / sigma).powi(2)).exp();
        alpha / 0.0081 * (-1.25 / (ratio * ratio)).exp() * gamma.max(1.0).powf(peak)
            / (k * k * k * k).max(1e-5)
    }

    fn directional_spreading(alignment: f32, omega: f32, peak_omega: f32, swell: f32) -> f32 {
        let ratio = (omega / peak_omega).max(1e-4);
        let base_s = if ratio <= 1.0 {
            6.97 * ratio.powi(5)
        } else {
            9.77 * ratio.powf(-2.5)
        };
        let s = base_s + 16.0 * (ratio.min(20.0)).tanh() * swell * swell;
        let s2 = s * s;
        let normalization = if s < 5.0 {
            -0.000564 * s2 * s2 + 0.00776 * s2 * s - 0.044 * s2 + 0.192 * s + 0.163
        } else {
            -4.80e-8 * s2 * s2 + 1.07e-5 * s2 * s - 9.53e-4 * s2 + 0.059 * s + 0.393
        };
        normalization * (0.5 * (1.0 + alignment)).max(0.0).powf(s)
    }

    fn opposite_index(index: u32, size: u32) -> u32 {
        (size - index) % size
    }

    fn conjugate(value: [f32; 2]) -> [f32; 2] {
        [value[0], -value[1]]
    }

    fn complex_mul(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
        [a[0] * b[0] - a[1] * b[1], a[0] * b[1] + a[1] * b[0]]
    }

    // Mirrors the shader's Tessendorf pair.
    fn evolve_pair(h0: [f32; 2], h0_opposite: [f32; 2], phase: [f32; 2]) -> [f32; 2] {
        let inverse_phase = conjugate(phase);
        let first = complex_mul(h0, phase);
        let second = complex_mul(conjugate(h0_opposite), inverse_phase);
        [first[0] + second[0], first[1] + second[1]]
    }

    fn evolve_time_derivative(
        h0: [f32; 2],
        h0_opposite: [f32; 2],
        phase: [f32; 2],
        omega: f32,
    ) -> [f32; 2] {
        let first = complex_mul(h0, phase);
        let second = complex_mul(conjugate(h0_opposite), conjugate(phase));
        [
            -(first[1] - second[1]) * omega,
            (first[0] - second[0]) * omega,
        ]
    }

    fn horizontal_jacobian_derivatives(
        height: f32,
        kx: f32,
        kz: f32,
        k_length: f32,
        chop: f32,
    ) -> [f32; 3] {
        let scale = chop / k_length;
        [
            height * kx * kx * scale,
            height * kx * kz * scale,
            height * kz * kz * scale,
        ]
    }

    #[test]
    fn opposite_index_is_an_involution_including_dc_and_nyquist() {
        let size = 256;
        for index in 0..size {
            assert_eq!(opposite_index(opposite_index(index, size), size), index);
        }
        assert_eq!(opposite_index(0, size), 0);
        assert_eq!(opposite_index(size / 2, size), size / 2);
    }

    #[test]
    fn tessendorf_pair_is_hermitian_for_regular_and_self_paired_bins() {
        let phase = [0.6, 0.8];
        let h0 = [1.25, -0.75];
        let h0_opposite = [-0.4, 0.9];
        let h = evolve_pair(h0, h0_opposite, phase);
        let opposite = evolve_pair(h0_opposite, h0, phase);
        assert!((opposite[0] - h[0]).abs() < 1e-6);
        assert!((opposite[1] + h[1]).abs() < 1e-6);

        let self_paired = evolve_pair(h0, h0, phase);
        assert!(self_paired[1].abs() < 1e-6);

        let dhdt = evolve_time_derivative(h0, h0_opposite, phase, 2.0);
        let dhdt_opposite = evolve_time_derivative(h0_opposite, h0, phase, 2.0);
        assert!((dhdt_opposite[0] - dhdt[0]).abs() < 1e-6);
        assert!((dhdt_opposite[1] + dhdt[1]).abs() < 1e-6);

        let self_paired_dhdt = evolve_time_derivative(h0, h0, phase, 2.0);
        assert!(self_paired_dhdt[1].abs() < 1e-6);
    }

    #[test]
    fn only_spectrum_inputs_invalidate_h0() {
        let mut params = crate::ffi::WgrWaterParams::zeroed();
        let initial = SpectrumInputs::from_params(&params);
        params.time = 42.0;
        params.wave_speed = 3.0;
        assert_eq!(SpectrumInputs::from_params(&params), initial);
        params.fft_control[1] = 1337.0;
        assert_ne!(SpectrumInputs::from_params(&params), initial);
    }

    #[test]
    fn cascade_bands_are_a_smooth_conservative_partition() {
        let lengths = [48.0, 144.0, 432.0, 1296.0];
        for step in 1..=400 {
            let k = 0.002 + step as f32 * 0.05;
            let bands = cascade_bands(k, lengths);
            assert!(bands.iter().all(|weight| (0.0..=1.0).contains(weight)));
            assert!((bands.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        }
        assert!(cascade_bands(0.005, lengths)[3] > 0.9);
        assert!(cascade_bands(1.0, lengths)[0] > 0.9);
    }

    #[test]
    fn swell_direction_is_seeded_and_cross_wind() {
        let angle = swell_angle(1337);
        assert_eq!(angle, swell_angle(1337));
        assert_ne!(angle, swell_angle(1338));
        assert!((0.45..=0.95).contains(&angle.abs()));
    }

    #[test]
    fn jonswap_shape_is_nonnegative_and_peak_enhancement_is_localized() {
        let peak_k = 9.81 / (11.0 * 11.0);
        let gamma_one = jonswap_k_shape(peak_k, peak_k, 0.0081, 1.0);
        let gamma_three = jonswap_k_shape(peak_k, peak_k, 0.0081, 3.0);
        assert!(gamma_one.is_finite() && gamma_one > 0.0);
        assert!(gamma_three > gamma_one * 2.9);
        assert!(jonswap_k_shape(peak_k * 0.1, peak_k, 0.0081, 3.0) < gamma_one);
        assert!(jonswap_k_shape(peak_k * 8.0, peak_k, 0.0081, 3.0) < gamma_one);
    }

    #[test]
    fn directional_spreading_prefers_the_wind_and_remains_nonnegative() {
        let peak_omega = (9.81_f32 * (9.81_f32 / (11.0_f32 * 11.0_f32))).sqrt();
        let forward = directional_spreading(1.0, peak_omega, peak_omega, 0.45);
        let crosswind = directional_spreading(0.0, peak_omega, peak_omega, 0.45);
        let backward = directional_spreading(-1.0, peak_omega, peak_omega, 0.45);
        assert!(forward.is_finite() && forward > crosswind && crosswind > backward);
        assert_eq!(backward, 0.0);
    }

    #[test]
    fn horizontal_displacement_derivatives_have_the_spectral_sign_and_jacobian() {
        // D = -i*h*k/|k|*chop, so dD/dx = i*k*D has h's positive sign.
        let derivatives = horizontal_jacobian_derivatives(2.0, 3.0, -4.0, 5.0, 0.25);
        assert!((derivatives[0] - 0.9).abs() < 1e-6);
        assert!((derivatives[1] + 1.2).abs() < 1e-6);
        assert!((derivatives[2] - 1.6).abs() < 1e-6);

        let jacobian =
            (1.0 + derivatives[0]) * (1.0 + derivatives[2]) - derivatives[1] * derivatives[1];
        assert!((jacobian - 3.5).abs() < 1e-6);
    }

    #[test]
    fn spectrum_shaders_validate() {
        for source in [
            include_str!("fft_spectrum_init.wgsl"),
            include_str!("fft_spectrum.wgsl"),
            include_str!("fft_compose.wgsl"),
            include_str!("foam.wgsl"),
        ] {
            let module = naga::front::wgsl::parse_str(source).expect("FFT spectrum WGSL parse");
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .expect("FFT spectrum WGSL validate");
        }
    }
}
