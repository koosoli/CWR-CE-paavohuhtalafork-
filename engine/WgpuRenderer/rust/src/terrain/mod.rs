use wgpu::util::DeviceExt;

use crate::ffi::{WgrTerrainNode, WgrTerrainParams};
use crate::gfx3d::DEPTH_FORMAT;
use crate::textures::TextureFormat;

// Grid mesh resolution: GRID_N quads per axis, (GRID_N+1)^2 vertices, u16 indices.
const GRID_N: u32 = 32;

pub struct Terrain
{
    group1_layout: wgpu::BindGroupLayout,
    group2_layout: wgpu::BindGroupLayout,

    params_ubo: wgpu::Buffer,
    #[allow(dead_code)] // kept alive: group1_bind references its view
    heightmap: wgpu::Texture,
    group1_bind: wgpu::BindGroup,
    have_heightmap: bool,

    #[allow(dead_code)] // kept alive: group2_bind references its view
    ground: wgpu::Texture,
    ground_sampler: wgpu::Sampler,
    group2_bind: wgpu::BindGroup,

    grid_vbuf: wgpu::Buffer,
    grid_ibuf: wgpu::Buffer,
    grid_index_count: u32,

    instance_buf: wgpu::Buffer,
    instance_cap: u64,
    instance_count: u32,

    pipeline: wgpu::RenderPipeline,
    bc_supported: bool,
    max_dim: u32,
}

impl Terrain
{
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
        bc_supported: bool,
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
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        // group 2 (fragment): ground texture array + filtering sampler.
        let group2_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_terrain_group2_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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

        let ground = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_terrain_ground"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texel_copy(&ground),
            &[0xFF, 0xFF, 0xFF, 0xFF],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let ground_view = ground.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let ground_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("wgr_terrain_ground_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let group2_bind = make_group2(device, &group2_layout, &ground_view, &ground_sampler);

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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_terrain_pipeline_layout"),
            bind_group_layouts: &[Some(camera_layout), Some(&group1_layout), Some(&group2_layout)],
            immediate_size: 0,
        });
        let grid_attrs = wgpu::vertex_attr_array![0 => Float32x2];
        let inst_attrs = wgpu::vertex_attr_array![1 => Float32x2, 2 => Float32, 3 => Uint32];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_terrain_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_terrain"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: 8,
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
                compilation_options: Default::default(),
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
            ground,
            ground_sampler,
            group2_bind,
            grid_vbuf,
            grid_ibuf,
            grid_index_count,
            instance_buf,
            instance_cap,
            instance_count: 0,
            pipeline,
            bc_supported,
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

    pub fn set_ground_textures(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer_count: u32,
        width: u32,
        height: u32,
        format: TextureFormat,
        data: &[u8],
    )
    {
        if layer_count == 0 || width == 0 || height == 0 {
            return;
        }
        if format.is_block_compressed() && !self.bc_supported {
            return;
        }
        let per_layer = format.expected_len(width, height) as usize;
        if data.len() < per_layer * layer_count as usize {
            return;
        }

        let ground = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_terrain_ground"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: layer_count },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.wgpu_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for layer in 0..layer_count {
            let start = layer as usize * per_layer;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &ground,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                    aspect: wgpu::TextureAspect::All,
                },
                &data[start..start + per_layer],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(format.bytes_per_row(width)),
                    rows_per_image: Some(format.rows(height)),
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        }
        let view = ground.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        self.group2_bind = make_group2(device, &self.group2_layout, &view, &self.ground_sampler);
        self.ground = ground;
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

fn make_group2(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    ground_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup
{
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("wgr_terrain_group2_bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(ground_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

// The reusable unit grid: (GRID_N+1)^2 vertices spanning [0,1]^2, two triangles
// per quad. Positions are the only per-vertex attribute; height comes from the
// vertex-shader heightmap sample.
fn build_grid(device: &wgpu::Device) -> (wgpu::Buffer, wgpu::Buffer, u32)
{
    let side = GRID_N + 1;
    let mut verts: Vec<[f32; 2]> = Vec::with_capacity((side * side) as usize);
    for z in 0..side {
        for x in 0..side {
            verts.push([x as f32 / GRID_N as f32, z as f32 / GRID_N as f32]);
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
