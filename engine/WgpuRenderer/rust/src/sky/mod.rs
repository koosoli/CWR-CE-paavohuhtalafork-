// Procedural atmospheric sky (plan Stage 2a): a fullscreen pass that marches the
// view ray through a Hillaire-style LUT atmosphere and writes the scene target
// (HDR texture when the HDR path is on, else the swapchain) BEFORE geometry, so
// terrain/objects overdraw it; depth is neither tested nor written. See
// docs/procedural-sky-plan.md.
//
// Two small LUTs feed the main march — a transmittance LUT and an isotropic
// multi-scattering LUT — both depending only on the atmosphere parameters (the sun
// is a LUT axis), so they rebuild only when those params change (dirty-flagged),
// not every frame. Celestial + authored params arrive from C++ via wgr_set_sky.

use bytemuck::Zeroable;
use wgpu::util::DeviceExt;

use crate::ffi::WgrSky;

// CPU reference port of the atmosphere math, for objective colour unit tests.
#[cfg(test)]
mod reference;

// Guards every sky.wgsl edit: parse + validate the module offline so a WGSL error
// surfaces in CI/tests instead of as a pipeline-creation panic at runtime.
#[test]
fn sky_wgsl_validates() {
    let module = naga::front::wgsl::parse_str(include_str!("sky.wgsl")).expect("sky.wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("sky.wgsl validate");
}

// LUT resolutions. Transmittance is smooth in both axes; multiscatter is very
// low-frequency, so a tiny map suffices (its build is the expensive one).
const TRANSMITTANCE_W: u32 = 256;
const TRANSMITTANCE_H: u32 = 64;
const MULTISCATTER_SIZE: u32 = 32;
const LUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

// Aerial-perspective froxel volume: XY = screen, Z = distance (squared distribution).
// 32^3 is Hillaire's classic size — cheap to fill, soft enough for god-ray shafts.
const FROXEL_W: u32 = 32;
const FROXEL_H: u32 = 32;
const FROXEL_D: u32 = 32;
const FROXEL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

// GPU uniform for the sky pass: the pushed WgrSky (8 vec4) plus the reconstructed
// inverse view-projection and an output-mode block. Must match `Sky` in sky.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkyUniform {
    inv_view_proj: [[f32; 4]; 4],
    sun_dir: [f32; 4],
    moon_dir: [f32; 4],
    rayleigh: [f32; 4],
    mie: [f32; 4],
    ground_albedo: [f32; 4],
    params: [f32; 4],
    control: [f32; 4],
    fog_color: [f32; 4],
    night_zenith: [f32; 4],
    night_horizon: [f32; 4],
    night_params: [f32; 4],
    // x = linear output (1 = write linear radiance for the tonemap resolve; 0 =
    // self-tonemap for the LDR-direct path). y/z/w reserved.
    output: [f32; 4],
    // xyz = absolute world camera position, so cs_froxel can turn a marched camera-
    // relative offset into a world position for the terrain sun-shadow mask lookup.
    cam_pos: [f32; 4],
}

// The atmosphere-only fields that determine the LUTs (sun/night/exposure excluded,
// since the sun is a LUT axis and those don't change the tables). A change here
// dirties the LUTs; per-frame celestial pushes do not.
type LutKey = [f32; 14];

fn lut_key(sky: &WgrSky) -> LutKey {
    [
        sky.rayleigh[0],
        sky.rayleigh[1],
        sky.rayleigh[2],
        sky.rayleigh[3],
        sky.mie[0],
        sky.mie[1],
        sky.mie[2],
        sky.mie[3],
        sky.ground_albedo[0],
        sky.ground_albedo[1],
        sky.ground_albedo[2],
        sky.params[2],  // planet radius
        sky.params[3],  // atmosphere thickness
        sky.control[3], // ozone strength
    ]
}

pub struct Sky {
    // Main fullscreen sky pipeline (targets the scene format).
    sky_pipeline: wgpu::RenderPipeline,
    sky_bind: wgpu::BindGroup,
    // LUT build pipelines (target LUT_FORMAT) + their target views/binds.
    transmittance_pipeline: wgpu::RenderPipeline,
    transmittance_bind: wgpu::BindGroup,
    transmittance_view: wgpu::TextureView,
    multiscatter_pipeline: wgpu::RenderPipeline,
    multiscatter_bind: wgpu::BindGroup,
    multiscatter_view: wgpu::TextureView,
    // Aerial-perspective froxel volume + its compute fill (see cs_froxel in sky.wgsl).
    // The `_tex` handle is held only to keep the texture alive; both bind groups view it.
    froxel_pipeline: wgpu::ComputePipeline,
    froxel_bind: wgpu::BindGroup,
    #[allow(dead_code)]
    froxel_tex: wgpu::Texture,
    froxel_view: wgpu::TextureView,
    // Group(1) of cs_froxel: the terrain sun-shadow mask, rebuilt each frame (the mask
    // texture is Terrain-owned and regenerated) from the lent view + these two.
    froxel_shadow_layout: wgpu::BindGroupLayout,
    mask_sampler: wgpu::Sampler,
    mapping_buf: wgpu::Buffer,
    csm_cmp_sampler: wgpu::Sampler,
    csm_ubo: wgpu::Buffer,
    uniform_buf: wgpu::Buffer,
    // 1 = scene target is the linear HDR texture (tonemap resolves later); 0 =
    // LDR-direct, so the sky self-tonemaps. Fixed at construction with the format.
    linear: f32,
    // LUTs are rebuilt only when the atmosphere key changes (None = never built).
    last_lut: Option<LutKey>,
    lut_dirty: bool,
}

impl Sky {
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_sky_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sky.wgsl").into()),
        });

        // Bind-group entry templates (binding numbers match sky.wgsl's globals).
        let uniform_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let sampler_entry = wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };

        // Per-pass layouts: transmittance reads only the uniform; multiscatter also
        // reads the transmittance LUT; the main pass reads both LUTs.
        let transmittance_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_sky_transmittance_layout"),
            entries: &[uniform_entry(0)],
        });
        let multiscatter_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_sky_multiscatter_layout"),
            entries: &[uniform_entry(0), sampler_entry, tex_entry(2)],
        });
        let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_sky_layout"),
            entries: &[uniform_entry(0), sampler_entry, tex_entry(2), tex_entry(3)],
        });
        let make_pipeline = |label: &str,
                             layout: &wgpu::BindGroupLayout,
                             fs: &str,
                             format: wgpu::TextureFormat| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
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
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let transmittance_pipeline = make_pipeline(
            "wgr_sky_transmittance",
            &transmittance_layout,
            "fs_transmittance",
            LUT_FORMAT,
        );
        let multiscatter_pipeline = make_pipeline(
            "wgr_sky_multiscatter",
            &multiscatter_layout,
            "fs_multiscatter",
            LUT_FORMAT,
        );
        let sky_pipeline = make_pipeline("wgr_sky", &sky_layout, "fs_sky", color_format);

        let make_lut = |label: &str, w: u32, h: u32| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: LUT_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            tex.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let transmittance_view = make_lut("wgr_sky_transmittance_lut", TRANSMITTANCE_W, TRANSMITTANCE_H);
        let multiscatter_view = make_lut("wgr_sky_multiscatter_lut", MULTISCATTER_SIZE, MULTISCATTER_SIZE);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_sky_lut_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_sky_uniform"),
            contents: bytemuck::bytes_of(&SkyUniform::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let transmittance_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_sky_transmittance_bind"),
            layout: &transmittance_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });
        let multiscatter_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_sky_multiscatter_bind"),
            layout: &multiscatter_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&transmittance_view),
                },
            ],
        });
        let sky_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_sky_bind"),
            layout: &sky_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&transmittance_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&multiscatter_view),
                },
            ],
        });

        // Froxel volume + compute fill. Same atmosphere inputs as the render passes
        // (uniform + both LUTs + sampler) at COMPUTE visibility, plus the 3D volume as a
        // write storage texture at binding 5. The volume is frustum-parameterised (not
        // screen-sized), so it is a fixed 32^3 built once here.
        let froxel_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_sky_froxel"),
            size: wgpu::Extent3d {
                width: FROXEL_W,
                height: FROXEL_H,
                depth_or_array_layers: FROXEL_D,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: FROXEL_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let froxel_view = froxel_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("wgr_sky_froxel_view"),
            dimension: Some(wgpu::TextureViewDimension::D3),
            ..Default::default()
        });

        let compute_uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let compute_sampler = wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let compute_tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let froxel_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_sky_froxel_layout"),
            entries: &[
                compute_uniform,
                compute_sampler,
                compute_tex(2),
                compute_tex(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FROXEL_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D3,
                    },
                    count: None,
                },
            ],
        });
        // Group(1): the terrain sun-shadow mask (texture + sampler) + its world->UV mapping
        // uniform, so cs_froxel can occlude the fog by terrain. The mask is Terrain-owned and
        // lent by view (regenerated on heightmap change), so this bind rebuilds each frame in
        // render_froxel; the sampler + mapping buffer are owned here and created once.
        let froxel_shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_sky_froxel_shadow_layout"),
            entries: &[
                compute_tex(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<crate::terrain::TerrainShadowMap>() as u64,
                        ),
                    },
                    count: None,
                },
                // Cascade shadow depth (D2Array) + comparison sampler + the cascade matrices,
                // so cs_froxel occludes the fog by objects/terrain casters for crisp shafts.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<crate::ffi::WgrCameraShadow>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });
        let csm_cmp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_sky_froxel_csm_sampler"),
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let csm_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_sky_froxel_csm"),
            size: std::mem::size_of::<crate::ffi::WgrCameraShadow>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_sky_froxel_mask_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mapping_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_sky_froxel_mapping"),
            size: std::mem::size_of::<crate::terrain::TerrainShadowMap>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let froxel_pipeline = {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("wgr_sky_froxel"),
                bind_group_layouts: &[Some(&froxel_layout), Some(&froxel_shadow_layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("wgr_sky_froxel"),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some("cs_froxel"),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let froxel_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_sky_froxel_bind"),
            layout: &froxel_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&transmittance_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&multiscatter_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&froxel_view),
                },
            ],
        });

        let linear = if color_format == crate::HDR_FORMAT { 1.0 } else { 0.0 };

        Self {
            sky_pipeline,
            sky_bind,
            transmittance_pipeline,
            transmittance_bind,
            transmittance_view,
            multiscatter_pipeline,
            multiscatter_bind,
            multiscatter_view,
            froxel_pipeline,
            froxel_bind,
            froxel_tex,
            froxel_view,
            froxel_shadow_layout,
            mask_sampler,
            mapping_buf,
            csm_cmp_sampler,
            csm_ubo,
            uniform_buf,
            linear,
            last_lut: None,
            lut_dirty: true,
        }
    }

    // Rebuild the sky uniform for this frame and flag the LUTs dirty if the
    // atmosphere parameters changed.
    pub fn upload(
        &mut self,
        queue: &wgpu::Queue,
        sky: &WgrSky,
        inv_view_proj: [[f32; 4]; 4],
        cam_pos: [f32; 4],
        shadow_map: &crate::terrain::TerrainShadowMap,
        csm: &crate::ffi::WgrCameraShadow,
    ) {
        let key = lut_key(sky);
        if self.last_lut != Some(key) {
            self.last_lut = Some(key);
            self.lut_dirty = true;
        }
        let u = SkyUniform {
            inv_view_proj,
            sun_dir: sky.sun_dir,
            moon_dir: sky.moon_dir,
            rayleigh: sky.rayleigh,
            mie: sky.mie,
            ground_albedo: sky.ground_albedo,
            params: sky.params,
            control: sky.control,
            fog_color: sky.fog_color,
            night_zenith: sky.night_zenith,
            night_horizon: sky.night_horizon,
            night_params: sky.night_params,
            output: [self.linear, 0.0, 0.0, 0.0],
            cam_pos,
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
        // Terrain sun-shadow mask mapping for cs_froxel's occlusion lookup (own copy so
        // the froxel fill doesn't depend on the graphics camera group's buffer).
        queue.write_buffer(&self.mapping_buf, 0, bytemuck::bytes_of(shadow_map));
        // The main camera's cascade matrices, for the froxel's near-field CSM occlusion.
        queue.write_buffer(&self.csm_ubo, 0, bytemuck::bytes_of(csm));
    }

    // Record the transmittance + multiscatter LUT passes when the atmosphere changed.
    // Must run before the main sky pass on the same encoder. `upload` must precede it.
    pub fn render_luts(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if !self.lut_dirty {
            return;
        }
        self.lut_dirty = false;
        encoder.push_debug_group("wgr_sky_luts");
        self.lut_pass(encoder, "wgr_sky_transmittance", &self.transmittance_pipeline,
                      &self.transmittance_bind, &self.transmittance_view);
        // Multiscatter samples the transmittance LUT just rendered, so it runs after.
        self.lut_pass(encoder, "wgr_sky_multiscatter", &self.multiscatter_pipeline,
                      &self.multiscatter_bind, &self.multiscatter_view);
        encoder.pop_debug_group();
    }

    fn lut_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
        pipeline: &wgpu::RenderPipeline,
        bind: &wgpu::BindGroup,
        view: &wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }

    // Draw the fullscreen sky into an already-begun render pass targeting the scene.
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.sky_pipeline);
        pass.set_bind_group(0, &self.sky_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    // Fill the aerial-perspective froxel volume for this frame (see cs_froxel). Reuses
    // this frame's sky uniform + LUTs, so `upload` + `render_luts` must have run first.
    // One thread per screen column marches the atmosphere front-to-back into the slices.
    pub fn render_froxel(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        shadow_mask_view: &wgpu::TextureView,
        csm_view: &wgpu::TextureView,
    ) {
        // Group(1) is rebuilt each frame because the terrain mask + CSM textures are owned
        // elsewhere and regenerated; cheap. upload() has already refreshed mapping_buf/csm_ubo.
        let shadow_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_sky_froxel_shadow_bind"),
            layout: &self.froxel_shadow_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(shadow_mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.mapping_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(csm_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.csm_cmp_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.csm_ubo.as_entire_binding(),
                },
            ],
        });
        encoder.push_debug_group("wgr_sky_froxel");
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_sky_froxel"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.froxel_pipeline);
        pass.set_bind_group(0, &self.froxel_bind, &[]);
        pass.set_bind_group(1, &shadow_bind, &[]);
        pass.dispatch_workgroups(FROXEL_W.div_ceil(8), FROXEL_H.div_ceil(8), 1);
        drop(pass);
        encoder.pop_debug_group();
    }

    // The froxel volume view, lent to the camera bind group so the forward shaders
    // sample it (frame::froxel_fog).
    pub fn froxel_view(&self) -> &wgpu::TextureView {
        &self.froxel_view
    }
}
