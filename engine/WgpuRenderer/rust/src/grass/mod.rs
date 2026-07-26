use crate::ffi::{WgrGrassParams, WgrGrassTrack, WgrTerrainParams, WGR_GRASS_TRACK_COUNT};
use crate::gfx3d::{DEPTH_FORMAT, NORMAL_FORMAT};
use crate::terrain::Terrain;

const GRID_DIM: u32 = 512;
const MAX_INSTANCES: u32 = GRID_DIM * GRID_DIM;
// A separate coarse ring follows the reference project's distance LOD idea,
// but stays GPU-generated and camera-snapped rather than using moving CPU tiles.
const FAR_GRID_DIM: u32 = 384;
const MAX_FAR_INSTANCES: u32 = FAR_GRID_DIM * FAR_GRID_DIM;
// The middle ring keeps a real blade silhouette between the dense cards and
// the single-triangle distance field.  It is intentionally separate from the
// far ring: a three-level field avoids the obvious 25-50 m quality cliff.
const MID_GRID_DIM: u32 = 384;
const MAX_MID_INSTANCES: u32 = MID_GRID_DIM * MID_GRID_DIM;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GrassParams {
    density: f32,
    spacing: f32,
    near_radius: f32,
    enabled: f32,
    blade_height: f32,
    wind_strength: f32,
    wind_direction: f32,
    far_radius: f32,
    interactor_x: f32,
    interactor_z: f32,
    interactor_radius: f32,
    interactor_strength: f32,
    tracks: [WgrGrassTrack; WGR_GRASS_TRACK_COUNT],
    debug_ignore_geography_exclusions: f32,
    _pad0: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GrassInstance {
    pos_seed: [f32; 4],
}

pub enum GrassPass {
    Color,
    ColorNoWrite,
    Prepass,
}

pub struct Grass {
    enabled: bool,
    terrain_params: wgpu::Buffer,
    grass_params: wgpu::Buffer,
    placement_count: wgpu::Buffer,
    indirect: wgpu::Buffer,
    mid_placement_count: wgpu::Buffer,
    mid_indirect: wgpu::Buffer,
    far_placement_count: wgpu::Buffer,
    far_indirect: wgpu::Buffer,
    terrain_layout: wgpu::BindGroupLayout,
    terrain_bind: wgpu::BindGroup,
    data_bind: wgpu::BindGroup,
    mid_data_bind: wgpu::BindGroup,
    far_data_bind: wgpu::BindGroup,
    heightmap_view: wgpu::TextureView,
    geography: wgpu::Texture,
    geography_view: wgpu::TextureView,
    terrain_generation: u64,
    have_heightmap: bool,
    place_pipeline: wgpu::ComputePipeline,
    mid_place_pipeline: wgpu::ComputePipeline,
    far_place_pipeline: wgpu::ComputePipeline,
    color_pipeline: wgpu::RenderPipeline,
    color_no_write_pipeline: wgpu::RenderPipeline,
    prepass_pipeline: wgpu::RenderPipeline,
    mid_prepass_pipeline: wgpu::RenderPipeline,
    mid_color_pipeline: wgpu::RenderPipeline,
    mid_color_no_write_pipeline: wgpu::RenderPipeline,
    far_color_pipeline: wgpu::RenderPipeline,
    far_color_no_write_pipeline: wgpu::RenderPipeline,
}

impl Grass {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
        sample_count: u32,
        composer: &mut naga_oil::compose::Composer,
    ) -> Self {
        let enabled = std::env::var("WGR_GRASS").map(|v| v != "0").unwrap_or(true);
        let terrain_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_terrain_params"),
            size: std::mem::size_of::<WgrTerrainParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let terrain_default = WgrTerrainParams {
            world_origin: glam::Vec2::ZERO,
            land_grid: 1.0,
            terrain_grid: 1.0,
            hm_width: 1,
            hm_height: 1,
            land_range: 1,
            data_scale: 1.0,
            sea_level: 0.0,
            time: 0.0,
            swash_speed: 0.0,
            swash_amp: 0.0,
            wet_height: 0.0,
            wet_darken: 1.0,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        queue.write_buffer(&terrain_params, 0, bytemuck::bytes_of(&terrain_default));
        let grass_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_params"),
            size: std::mem::size_of::<GrassParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let density = std::env::var("WGR_GRASS_DENSITY")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.99)
            .clamp(0.0, 1.0);
        let radius = std::env::var("WGR_GRASS_DISTANCE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(50.0)
            .clamp(8.0, 60.0);
        let params = GrassParams {
            density,
            // Dense 0.25 m grid; the 512x512 placement buffer supports this
            // without clipping the visible field at the old 65,536-instance cap.
            spacing: 0.25,
            near_radius: radius,
            enabled: if enabled { 1.0 } else { 0.0 },
            blade_height: 1.0,
            wind_strength: 0.75,
            wind_direction: 0.0,
            far_radius: radius,
            interactor_x: 0.0,
            interactor_z: 0.0,
            interactor_radius: 0.0,
            interactor_strength: 0.0,
            tracks: [WgrGrassTrack { x: 0.0, z: 0.0, radius: 0.0, age: 0.0 }; WGR_GRASS_TRACK_COUNT],
            debug_ignore_geography_exclusions: 0.0,
            _pad0: [0.0; 3],
        };
        queue.write_buffer(&grass_params, 0, bytemuck::bytes_of(&params));

        let heightmap = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_grass_heightmap_standin"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &heightmap,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::bytes_of(&0.0f32),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let heightmap_view = heightmap.create_view(&wgpu::TextureViewDescriptor::default());
        let geography = make_geography_texture(device, 1, 1);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &geography,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::bytes_of(&0u32),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let geography_view = geography.create_view(&wgpu::TextureViewDescriptor::default());

        let terrain_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_grass_terrain_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<WgrTerrainParams>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_instances"),
            size: (MAX_INSTANCES as usize * std::mem::size_of::<GrassInstance>()) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let mid_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_mid_instances"),
            size: (MAX_MID_INSTANCES as usize * std::mem::size_of::<GrassInstance>()) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let far_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_far_instances"),
            size: (MAX_FAR_INSTANCES as usize * std::mem::size_of::<GrassInstance>()) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let placement_count = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_placement_count"),
            // Binding 2's declared minimum is 16 B; only word 0 is the atomic
            // instance count, while the remaining words are padding.
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_indirect"),
            size: 16,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mid_placement_count = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_mid_placement_count"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mid_indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_mid_indirect"),
            size: 16,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let far_placement_count = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_far_placement_count"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let far_indirect = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_grass_far_indirect"),
            size: 16,
            usage: wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let data_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_grass_data_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX
                        | wgpu::ShaderStages::FRAGMENT
                        | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<GrassParams>() as u64
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<GrassInstance>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
            ],
        });
        let terrain_bind = make_terrain_bind(
            device,
            &terrain_layout,
            &terrain_params,
            &heightmap_view,
            &geography_view,
        );
        let data_bind = make_data_bind(
            device,
            &data_layout,
            &grass_params,
            &instances,
            &placement_count,
        );
        let mid_data_bind = make_data_bind(
            device,
            &data_layout,
            &grass_params,
            &mid_instances,
            &mid_placement_count,
        );
        let far_data_bind = make_data_bind(
            device,
            &data_layout,
            &grass_params,
            &far_instances,
            &far_placement_count,
        );
        let shader = crate::shaders::make_module(
            device,
            composer,
            "wgr_grass_shader",
            include_str!("grass.wgsl"),
            "grass/grass.wgsl",
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_grass_pipeline_layout"),
            bind_group_layouts: &[
                Some(camera_layout),
                Some(&terrain_layout),
                Some(&data_layout),
            ],
            immediate_size: 0,
        });
        let place_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("wgr_grass_place"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("cs_place"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mid_place_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("wgr_grass_place_mid"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("cs_place_mid"),
            compilation_options: Default::default(),
            cache: None,
        });
        let far_place_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("wgr_grass_place_far"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("cs_place_far"),
            compilation_options: Default::default(),
            cache: None,
        });
        let make_pipeline =
            |label: &str, vertex_entry: &str, entry: &str, target: wgpu::TextureFormat, depth_write: bool| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some(vertex_entry),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        cull_mode: None,
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: DEPTH_FORMAT,
                        depth_write_enabled: Some(depth_write),
                        depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                        stencil: Default::default(),
                        bias: Default::default(),
                    }),
                    multisample: wgpu::MultisampleState {
                        count: sample_count,
                        ..Default::default()
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: target,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let color_pipeline = make_pipeline("wgr_grass_color", "vs_grass", "fs_grass", surface_format, true);
        let color_no_write_pipeline = make_pipeline(
            "wgr_grass_color_no_write",
            "vs_grass",
            "fs_grass",
            surface_format,
            false,
        );
        let prepass_pipeline =
            make_pipeline("wgr_grass_prepass", "vs_grass", "fs_grass_prepass", NORMAL_FORMAT, true);
        let mid_prepass_pipeline =
            make_pipeline("wgr_grass_mid_prepass", "vs_grass_mid", "fs_grass_prepass", NORMAL_FORMAT, true);
        let mid_color_pipeline =
            make_pipeline("wgr_grass_mid_color", "vs_grass_mid", "fs_grass", surface_format, true);
        let mid_color_no_write_pipeline = make_pipeline(
            "wgr_grass_mid_color_no_write",
            "vs_grass_mid",
            "fs_grass",
            surface_format,
            false,
        );
        let far_color_pipeline = make_pipeline("wgr_grass_far_color", "vs_grass_far", "fs_grass", surface_format, false);
        let far_color_no_write_pipeline = make_pipeline(
            "wgr_grass_far_color_no_write",
            "vs_grass_far",
            "fs_grass",
            surface_format,
            false,
        );
        Self {
            enabled,
            terrain_params,
            grass_params,
            placement_count,
            indirect,
            mid_placement_count,
            mid_indirect,
            far_placement_count,
            far_indirect,
            terrain_layout,
            terrain_bind,
            data_bind,
            mid_data_bind,
            far_data_bind,
            heightmap_view,
            geography,
            geography_view,
            terrain_generation: u64::MAX,
            have_heightmap: false,
            place_pipeline,
            mid_place_pipeline,
            far_place_pipeline,
            color_pipeline,
            color_no_write_pipeline,
            prepass_pipeline,
            mid_prepass_pipeline,
            mid_color_pipeline,
            mid_color_no_write_pipeline,
            far_color_pipeline,
            far_color_no_write_pipeline,
        }
    }

    pub fn set_geography(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        values: &[u32],
    ) {
        if width == 0 || height == 0 || values.len() < width as usize * height as usize {
            return;
        }
        let geography = make_geography_texture(device, width, height);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &geography,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&values[..width as usize * height as usize]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.geography = geography;
        self.geography_view = self
            .geography
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.terrain_bind = make_terrain_bind(
            device,
            &self.terrain_layout,
            &self.terrain_params,
            &self.heightmap_view,
            &self.geography_view,
        );
    }

    pub fn set_params(&mut self, queue: &wgpu::Queue, params: WgrGrassParams) {
        let params = GrassParams {
            density: params.density.clamp(0.0, 1.0),
            spacing: params.spacing.clamp(0.10, 0.75),
            near_radius: params.near_radius.clamp(8.0, 128.0),
            enabled: if params.enabled != 0.0 { 1.0 } else { 0.0 },
            blade_height: params.blade_height.clamp(0.10, 3.0),
            wind_strength: params.wind_strength.clamp(0.0, 3.0),
            wind_direction: params.wind_direction,
            far_radius: params.far_radius.clamp(8.0, 5000.0),
            interactor_x: params.interactor_x,
            interactor_z: params.interactor_z,
            interactor_radius: params.interactor_radius.clamp(0.0, 16.0),
            interactor_strength: params.interactor_strength.clamp(0.0, 1.0),
            tracks: params.tracks,
            debug_ignore_geography_exclusions: params.debug_ignore_geography_exclusions,
            _pad0: params._pad0,
        };
        self.enabled = params.enabled != 0.0;
        queue.write_buffer(&self.grass_params, 0, bytemuck::bytes_of(&params));
    }

    pub fn prepare_terrain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        terrain: &Terrain,
    ) {
        let params = terrain.params();
        queue.write_buffer(&self.terrain_params, 0, bytemuck::bytes_of(&params));
        self.have_heightmap = terrain.has_heightmap();
        if self.terrain_generation != terrain.heightmap_gen() {
            self.terrain_generation = terrain.heightmap_gen();
            self.heightmap_view = terrain.heightmap_view();
            self.terrain_bind = make_terrain_bind(
                device,
                &self.terrain_layout,
                &self.terrain_params,
                &self.heightmap_view,
                &self.geography_view,
            );
        }
    }

    pub fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        camera_bind: &wgpu::BindGroup,
        camera_offset: u32,
    ) {
        if !self.enabled || !self.have_heightmap {
            return;
        }
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_grass_place"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.place_pipeline);
        pass.set_bind_group(0, camera_bind, &[camera_offset]);
        pass.set_bind_group(1, &self.terrain_bind, &[]);
        pass.set_bind_group(2, &self.data_bind, &[]);
        pass.dispatch_workgroups(GRID_DIM / 8, GRID_DIM / 8, 1);
        drop(pass);
        let mut mid_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_grass_place_mid"),
            timestamp_writes: None,
        });
        mid_pass.set_pipeline(&self.mid_place_pipeline);
        mid_pass.set_bind_group(0, camera_bind, &[camera_offset]);
        mid_pass.set_bind_group(1, &self.terrain_bind, &[]);
        mid_pass.set_bind_group(2, &self.mid_data_bind, &[]);
        mid_pass.dispatch_workgroups(MID_GRID_DIM / 8, MID_GRID_DIM / 8, 1);
        drop(mid_pass);
        let mut far_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("wgr_grass_place_far"),
            timestamp_writes: None,
        });
        far_pass.set_pipeline(&self.far_place_pipeline);
        far_pass.set_bind_group(0, camera_bind, &[camera_offset]);
        far_pass.set_bind_group(1, &self.terrain_bind, &[]);
        far_pass.set_bind_group(2, &self.far_data_bind, &[]);
        far_pass.dispatch_workgroups(FAR_GRID_DIM / 8, FAR_GRID_DIM / 8, 1);
        drop(far_pass);
        // The render pass consumes a dedicated indirect-argument buffer. Keeping the
        // atomic placement counter separate avoids a STORAGE_READ_WRITE/INDIRECT
        // conflict in wgpu's command-encoder validation.
        encoder.copy_buffer_to_buffer(&self.placement_count, 0, &self.indirect, 4, 4);
        encoder.copy_buffer_to_buffer(&self.mid_placement_count, 0, &self.mid_indirect, 4, 4);
        encoder.copy_buffer_to_buffer(&self.far_placement_count, 0, &self.far_indirect, 4, 4);
    }

    pub fn reset_indirect(&self, queue: &wgpu::Queue) {
        queue.write_buffer(&self.placement_count, 0, bytemuck::bytes_of(&0u32));
        queue.write_buffer(&self.mid_placement_count, 0, bytemuck::bytes_of(&0u32));
        queue.write_buffer(&self.far_placement_count, 0, bytemuck::bytes_of(&0u32));
        // Two crossed five-segment blade ribbons (2 cards * 5 quads * 6 verts).
        queue.write_buffer(&self.indirect, 0, bytemuck::cast_slice(&[60u32, 0, 0, 0]));
        // Two crossed two-segment ribbons (2 cards * 2 quads * 6 verts).
        queue.write_buffer(&self.mid_indirect, 0, bytemuck::cast_slice(&[24u32, 0, 0, 0]));
        // Far LOD is a single cheap triangle per compacted instance.
        queue.write_buffer(&self.far_indirect, 0, bytemuck::cast_slice(&[3u32, 0, 0, 0]));
    }

    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        camera_bind: &wgpu::BindGroup,
        camera_offset: u32,
        kind: GrassPass,
    ) {
        if !self.enabled || !self.have_heightmap {
            return;
        }
        let pipeline = match kind {
            GrassPass::Color => &self.color_pipeline,
            GrassPass::ColorNoWrite => &self.color_no_write_pipeline,
            GrassPass::Prepass => &self.prepass_pipeline,
        };
        // Far grass skips the normal/depth prepass: it is a sparse, one-triangle
        // visual fill. Draw it before the dense near cards so near blades retain
        // normal depth writing and naturally cover the transition.
        if !matches!(kind, GrassPass::Prepass) {
            let far_pipeline = match kind {
                GrassPass::Color => &self.far_color_pipeline,
                GrassPass::ColorNoWrite => &self.far_color_no_write_pipeline,
                GrassPass::Prepass => unreachable!(),
            };
            pass.set_pipeline(far_pipeline);
            pass.set_bind_group(0, camera_bind, &[camera_offset]);
            pass.set_bind_group(1, &self.terrain_bind, &[]);
            pass.set_bind_group(2, &self.far_data_bind, &[]);
            pass.draw_indirect(&self.far_indirect, 0);
        }
        let mid_pipeline = match kind {
            GrassPass::Color => &self.mid_color_pipeline,
            GrassPass::ColorNoWrite => &self.mid_color_no_write_pipeline,
            GrassPass::Prepass => &self.mid_prepass_pipeline,
        };
        pass.set_pipeline(mid_pipeline);
        pass.set_bind_group(0, camera_bind, &[camera_offset]);
        pass.set_bind_group(1, &self.terrain_bind, &[]);
        pass.set_bind_group(2, &self.mid_data_bind, &[]);
        pass.draw_indirect(&self.mid_indirect, 0);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, camera_bind, &[camera_offset]);
        pass.set_bind_group(1, &self.terrain_bind, &[]);
        pass.set_bind_group(2, &self.data_bind, &[]);
        pass.draw_indirect(&self.indirect, 0);
    }
}

fn make_geography_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgr_grass_geography"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn make_terrain_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    heightmap: &wgpu::TextureView,
    geography: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("wgr_grass_terrain_bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(heightmap),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(geography),
            },
        ],
    })
}

fn make_data_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    instances: &wgpu::Buffer,
    placement_count: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("wgr_grass_data_bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: instances.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: placement_count.as_entire_binding(),
            },
        ],
    })
}
