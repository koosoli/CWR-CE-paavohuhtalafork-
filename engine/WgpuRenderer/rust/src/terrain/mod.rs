use wgpu::util::DeviceExt;

use crate::ffi::{WgrTerrainNode, WgrTerrainParams};
use crate::gfx3d::DEPTH_FORMAT;

// Grid mesh resolution: GRID_N quads per axis, (GRID_N+1)^2 vertices, u16 indices.
const GRID_N: u32 = 32;

// Capacity of the ground binding_array (and the device binding-array limit we
// request). Must match WGR_TERRAIN_MAX_GROUND_LAYERS in wgpu_renderer.hpp.
pub const TERRAIN_MAX_GROUND_LAYERS: u32 = 512;

pub struct Terrain
{
    group1_layout: wgpu::BindGroupLayout,
    group2_layout: wgpu::BindGroupLayout,

    params_ubo: wgpu::Buffer,
    #[allow(dead_code)] // kept alive: group1_bind references its view
    heightmap: wgpu::Texture,
    group1_bind: wgpu::BindGroup,
    have_heightmap: bool,

    // group2 resources; group2_bind holds views into all of them. Kept so any
    // one can be replaced and the bind group rebuilt.
    ground_views: Vec<wgpu::TextureView>,
    // Fills unused binding_array slots when PARTIALLY_BOUND is unavailable.
    pad_view: wgpu::TextureView,
    partially_bound: bool,
    index_map: wgpu::Texture,
    detail_view: wgpu::TextureView,
    jitter_map: wgpu::Texture,
    ground_sampler: wgpu::Sampler,
    ground_clamp_sampler: wgpu::Sampler,
    group2_bind: wgpu::BindGroup,

    grid_vbuf: wgpu::Buffer,
    grid_ibuf: wgpu::Buffer,
    grid_index_count: u32,

    instance_buf: wgpu::Buffer,
    instance_cap: u64,
    instance_count: u32,

    pipeline: wgpu::RenderPipeline,
    max_dim: u32,
}

impl Terrain
{
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
        partially_bound: bool,
        white_view: wgpu::TextureView,
    ) -> Self
    {
        // group 1 (vertex): terrain params UBO + heightmap (R32Float, textureLoad).
        let group1_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_terrain_group1_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Vertex reads grid/terrain spacing; fragment reads land_grid for tiling UVs.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
                    // Vertex displaces by the height; fragment reads it again for a
                    // per-pixel, LOD/morph-independent normal (even lighting).
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        // group 2 (fragment): bindless ground texture binding_array + filtering
        // sampler + per-cell index map (uint, textureLoad) + high-frequency
        // detail noise texture + an edge-extending sampler for clamped
        // transition tiles (index-map bit 15) + per-grid-point jitter map.
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_terrain_group2_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: Some(std::num::NonZeroU32::new(TERRAIN_MAX_GROUND_LAYERS).unwrap()),
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
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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

        let params_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_terrain_params"),
            size: std::mem::size_of::<WgrTerrainParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Nonzero defaults so the shader never divides by zero before the first upload.
        let default_params = WgrTerrainParams {
            world_origin: glam::Vec2::ZERO,
            land_grid: 1.0,
            terrain_grid: 1.0,
            hm_width: 1,
            hm_height: 1,
            land_range: 1,
            data_scale: 1.0,
        };
        queue.write_buffer(&params_ubo, 0, bytemuck::bytes_of(&default_params));

        // 1x1 stand-in heightmap + ground array so the bind groups are valid before
        // any upload; terrain never draws until a real heightmap arrives.
        let (heightmap, heightmap_view) = create_heightmap(device, 1, 1);
        queue.write_texture(
            texel_copy(&heightmap),
            bytemuck::bytes_of(&0.0f32),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let group1_bind = make_group1(device, &group1_layout, &params_ubo, &heightmap_view);

        // Stand-ins so the bind group is valid before any upload. Ground = the
        // shared 1x1 white; detail alpha = 0.5 (so the shader's 2*alpha
        // modulation is a no-op); index map = layer 0.
        let ground_views = vec![white_view.clone()];
        let index_map = create_index_map(device, 1, 1);
        queue.write_texture(
            texel_copy(&index_map),
            bytemuck::bytes_of(&0u16),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(2),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let detail = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_terrain_detail_neutral"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texel_copy(&detail),
            &[0x80, 0x80, 0x80, 0x80],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        // The view keeps the neutral stand-in texture alive.
        let detail_view = detail.create_view(&wgpu::TextureViewDescriptor::default());
        // Zero jitter until a real map arrives.
        let jitter_map = create_jitter_map(device, 1, 1);
        queue.write_texture(
            texel_copy(&jitter_map),
            &[0u8, 0u8],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(2),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        // 16x anisotropy: terrain is the worst case for isotropic mip selection
        // (large planes at grazing angles), and GL33's samplers already use it.
        let ground_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_terrain_ground_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        // For clamped transition tiles: edge-extends the tile past its own cell
        // (GL33's ClampU|ClampV) instead of wrapping.
        let ground_clamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_terrain_ground_clamp_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 16,
            ..Default::default()
        });
        let group2_bind = make_group2(
            device,
            &group2_layout,
            &ground_views,
            &white_view,
            partially_bound,
            &index_map,
            &detail_view,
            &jitter_map,
            &ground_sampler,
            &ground_clamp_sampler,
        );

        let (grid_vbuf, grid_ibuf, grid_index_count) = build_grid(device);

        let instance_cap = 64 * std::mem::size_of::<WgrTerrainNode>() as u64;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_terrain_instances"),
            size: instance_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_terrain_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("terrain.wgsl").into()),
        });
        // Override constants baked from the environment (see terrain.wgsl).
        let blend_width = std::env::var("WGR_TERRAIN_BLEND_WIDTH")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.15);
        let skirt_k = std::env::var("WGR_TERRAIN_SKIRT_K")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(4.0);
        let vs_constants = [("skirt_k", skirt_k)];
        let fs_constants = [("blend_width", blend_width)];
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_terrain_pipeline_layout"),
            bind_group_layouts: &[Some(camera_layout), Some(&group1_layout), Some(&group2_layout)],
            immediate_size: 0,
        });
        let grid_attrs = wgpu::vertex_attr_array![0 => Float32x3];
        let inst_attrs =
            wgpu::vertex_attr_array![1 => Float32x2, 2 => Float32, 3 => Uint32, 4 => Float32x2];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_terrain_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_terrain"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &vs_constants,
                    ..Default::default()
                },
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: 12,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &grid_attrs,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<WgrTerrainNode>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &inst_attrs,
                    },
                ],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                // Reversed-Z: nearer geometry has the larger depth value.
                depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_terrain"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &fs_constants,
                    ..Default::default()
                },
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Terrain {
            group1_layout,
            group2_layout,
            params_ubo,
            heightmap,
            group1_bind,
            have_heightmap: false,
            ground_views,
            pad_view: white_view,
            partially_bound,
            index_map,
            detail_view,
            jitter_map,
            ground_sampler,
            ground_clamp_sampler,
            group2_bind,
            grid_vbuf,
            grid_ibuf,
            grid_index_count,
            instance_buf,
            instance_cap,
            instance_count: 0,
            pipeline,
            max_dim: device.limits().max_texture_dimension_2d,
        }
    }

    pub fn set_heightmap(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        heights: &[f32],
        params: WgrTerrainParams,
    )
    {
        let (w, h) = (params.hm_width, params.hm_height);
        if w == 0 || h == 0 || w > self.max_dim || h > self.max_dim {
            return;
        }
        if heights.len() < (w as usize * h as usize) {
            return;
        }

        let (heightmap, view) = create_heightmap(device, w, h);
        queue.write_texture(
            texel_copy(&heightmap),
            bytemuck::cast_slice(&heights[..(w as usize * h as usize)]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.write_buffer(&self.params_ubo, 0, bytemuck::bytes_of(&params));
        self.group1_bind = make_group1(device, &self.group1_layout, &self.params_ubo, &view);
        self.heightmap = heightmap;
        self.have_heightmap = true;
    }

    // Ground layers as views into the shared texture registry (missing handles
    // already resolved to the white fallback by the caller). Truncated to the
    // binding_array capacity; the index-map upload clamps cell indices to match.
    pub fn set_ground_layers(&mut self, device: &wgpu::Device, mut views: Vec<wgpu::TextureView>)
    {
        views.truncate(TERRAIN_MAX_GROUND_LAYERS as usize);
        if views.is_empty() {
            views.push(self.pad_view.clone());
        }
        self.ground_views = views;
        self.rebuild_group2(device);
    }

    pub fn set_index_map(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        indices: &[u16],
    )
    {
        if width == 0 || height == 0 || width > self.max_dim || height > self.max_dim {
            return;
        }
        if indices.len() < width as usize * height as usize {
            return;
        }
        let index_map = create_index_map(device, width, height);
        queue.write_texture(
            texel_copy(&index_map),
            bytemuck::cast_slice(&indices[..width as usize * height as usize]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 2),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.index_map = index_map;
        self.rebuild_group2(device);
    }

    pub fn set_jitter_map(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        offsets: &[i8],
    )
    {
        if width == 0 || height == 0 || width > self.max_dim || height > self.max_dim {
            return;
        }
        if offsets.len() < 2 * width as usize * height as usize {
            return;
        }
        let jitter_map = create_jitter_map(device, width, height);
        queue.write_texture(
            texel_copy(&jitter_map),
            bytemuck::cast_slice(&offsets[..2 * width as usize * height as usize]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 2),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.jitter_map = jitter_map;
        self.rebuild_group2(device);
    }

    // Detail noise as a view into the shared texture registry.
    pub fn set_detail_layer(&mut self, device: &wgpu::Device, view: wgpu::TextureView)
    {
        self.detail_view = view;
        self.rebuild_group2(device);
    }

    fn rebuild_group2(&mut self, device: &wgpu::Device)
    {
        self.group2_bind = make_group2(
            device,
            &self.group2_layout,
            &self.ground_views,
            &self.pad_view,
            self.partially_bound,
            &self.index_map,
            &self.detail_view,
            &self.jitter_map,
            &self.ground_sampler,
            &self.ground_clamp_sampler,
        );
    }

    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, nodes: &[WgrTerrainNode])
    {
        self.instance_count = nodes.len() as u32;
        if nodes.is_empty() {
            return;
        }
        let needed = std::mem::size_of_val(nodes) as u64;
        if needed > self.instance_cap {
            let cap = needed.next_power_of_two();
            self.instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_terrain_instances"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_cap = cap;
        }
        queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(nodes));
    }

    // Draw one batch (a [first_node, first_node+node_count) run of the prepared
    // instances) with the given camera bind group + dynamic offset.
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        camera_bind: &wgpu::BindGroup,
        camera_offset: u32,
        first_node: u32,
        node_count: u32,
    )
    {
        if !self.have_heightmap || node_count == 0 {
            return;
        }
        if first_node + node_count > self.instance_count {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, camera_bind, &[camera_offset]);
        pass.set_bind_group(1, &self.group1_bind, &[]);
        pass.set_bind_group(2, &self.group2_bind, &[]);
        pass.set_vertex_buffer(0, self.grid_vbuf.slice(..));
        pass.set_vertex_buffer(1, self.instance_buf.slice(..));
        pass.set_index_buffer(self.grid_ibuf.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.grid_index_count, 0, first_node..first_node + node_count);
    }
}

fn texel_copy(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_>
{
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}

fn create_heightmap(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView)
{
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgr_terrain_heightmap"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn make_group1(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    params: &wgpu::Buffer,
    heightmap_view: &wgpu::TextureView,
) -> wgpu::BindGroup
{
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("wgr_terrain_group1_bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(heightmap_view),
            },
        ],
    })
}

fn create_index_map(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture
{
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgr_terrain_index_map"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

// Per-grid-point ground UV jitter (Landscape::_random), snorm UV offsets.
fn create_jitter_map(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture
{
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgr_terrain_jitter_map"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rg8Snorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

#[allow(clippy::too_many_arguments)]
fn make_group2(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    ground_views: &[wgpu::TextureView],
    pad_view: &wgpu::TextureView,
    partially_bound: bool,
    index_map: &wgpu::Texture,
    detail_view: &wgpu::TextureView,
    jitter_map: &wgpu::Texture,
    sampler: &wgpu::Sampler,
    clamp_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup
{
    // Without PARTIALLY_BOUND_BINDING_ARRAY every declared slot must be bound,
    // so pad the tail; the shader never indexes past the real layer count.
    let mut ground_refs: Vec<&wgpu::TextureView> = ground_views.iter().collect();
    if !partially_bound {
        ground_refs.resize(TERRAIN_MAX_GROUND_LAYERS as usize, pad_view);
    }
    let index_view = index_map.create_view(&wgpu::TextureViewDescriptor::default());
    let jitter_view = jitter_map.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("wgr_terrain_group2_bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureViewArray(&ground_refs),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&index_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(detail_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(clamp_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(&jitter_view),
            },
        ],
    })
}

// The reusable unit grid: (GRID_N+1)^2 vertices over [0,1]^2, two triangles per
// quad, plus a border skirt. Vertex is (u, v, skirt); the shader drops skirt
// vertices below the surface to wall off LOD-transition cracks.
fn build_grid(device: &wgpu::Device) -> (wgpu::Buffer, wgpu::Buffer, u32)
{
    let side = GRID_N + 1;
    let unit = 1.0 / GRID_N as f32;
    let mut verts: Vec<[f32; 3]> = Vec::with_capacity((side * side) as usize);
    for z in 0..side {
        for x in 0..side {
            verts.push([x as f32 * unit, z as f32 * unit, 0.0]);
        }
    }
    let mut indices: Vec<u16> = Vec::with_capacity((GRID_N * GRID_N * 6) as usize);
    for z in 0..GRID_N {
        for x in 0..GRID_N {
            let i0 = (z * side + x) as u16;
            let i1 = i0 + 1;
            let i2 = i0 + side as u16;
            let i3 = i2 + 1;
            indices.extend_from_slice(&[i0, i2, i1, i1, i2, i3]);
        }
    }

    // One skirt wall per border edge segment: two triangles joining the edge pair
    // to a dropped duplicate. Winding is irrelevant (the pipeline culls nothing).
    let mut wall = |tops: [u16; 2], a: [f32; 2], b: [f32; 2]| {
        let s0 = verts.len() as u16;
        verts.push([a[0], a[1], 1.0]);
        let s1 = verts.len() as u16;
        verts.push([b[0], b[1], 1.0]);
        indices.extend_from_slice(&[tops[0], s0, tops[1], tops[1], s0, s1]);
    };
    for i in 0..GRID_N {
        let f = i as f32 * unit;
        let g = (i + 1) as f32 * unit;
        let b = GRID_N * side;
        // top (z=0) / bottom (z=GRID_N)
        wall([i as u16, (i + 1) as u16], [f, 0.0], [g, 0.0]);
        wall([(b + i) as u16, (b + i + 1) as u16], [f, 1.0], [g, 1.0]);
        // left (x=0) / right (x=GRID_N)
        wall([(i * side) as u16, ((i + 1) * side) as u16], [0.0, f], [0.0, g]);
        wall([(i * side + GRID_N) as u16, ((i + 1) * side + GRID_N) as u16], [1.0, f], [1.0, g]);
    }

    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("wgr_terrain_grid_vbuf"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("wgr_terrain_grid_ibuf"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    (vbuf, ibuf, indices.len() as u32)
}
