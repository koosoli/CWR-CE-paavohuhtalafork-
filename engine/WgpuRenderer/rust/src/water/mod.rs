use wgpu::util::DeviceExt;

use crate::ffi::{WgrWaterNode, WgrWaterParams};
use crate::gfx3d::DEPTH_FORMAT;

// Grid mesh resolution: GRID_N quads per axis, (GRID_N+1)^2 vertices, u16 indices.
// Must match GRID_N in water.wgsl (and the terrain grid, so the two can eventually
// share a mesh).
const GRID_N: u32 = 32;

// A flat GPU CDLOD water surface: the shared grid mesh instanced per selected node,
// placed on a horizontal plane at the frame's sea level, drawn after opaque terrain +
// 3D and depth-cut by coastlines. Deliberately trimmed vs. Terrain — no heightmap,
// ground array, index/jitter maps or shadow sweep; water needs none of them here.
pub struct Water {
    params_ubo: wgpu::Buffer,
    group1_bind: wgpu::BindGroup,
    grid_vbuf: wgpu::Buffer,
    grid_ibuf: wgpu::Buffer,
    grid_index_count: u32,
    instance_buf: wgpu::Buffer,
    instance_cap: u64,
    instance_count: u32,
    pipeline: wgpu::RenderPipeline,
    // Set once wgr_water_set_params has run (i.e. a map is loaded); until then there
    // is nothing sensible to draw.
    have_params: bool,
}

impl Water {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        camera_layout: &wgpu::BindGroupLayout,
        surface_format: wgpu::TextureFormat,
        composer: &mut naga_oil::compose::Composer,
    ) -> Water {
        let group1_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgr_water_group1_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let params_ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_water_params"),
            size: std::mem::size_of::<WgrWaterParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let default_params = WgrWaterParams {
            world_origin: crate::ffi::WgrVec2 { x: 0.0, y: 0.0 },
            terrain_grid: 1.0,
            sea_level: 0.0,
            hm_width: 1,
            hm_height: 1,
            _pad0: 0,
            _pad1: 0,
        };
        queue.write_buffer(&params_ubo, 0, bytemuck::bytes_of(&default_params));

        let group1_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_water_group1_bind"),
            layout: &group1_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_ubo.as_entire_binding(),
            }],
        });

        let (grid_vbuf, grid_ibuf, grid_index_count) = build_grid(device);

        let instance_cap = 64 * std::mem::size_of::<WgrWaterNode>() as u64;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_water_instances"),
            size: instance_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader = crate::shaders::make_module(
            device,
            composer,
            "wgr_water_shader",
            include_str!("water.wgsl"),
            "water/water.wgsl",
        );
        // Flat water needs no skirts: adjacent LOD levels are coplanar at sea level, so
        // there are no cracks to wall off — and with a transparent surface the skirt
        // walls show through as vertical seams between tiles. Default them off (0 =
        // skirt verts collapse onto the plane). The sibling wave plan can raise this.
        let skirt_k = std::env::var("WGR_WATER_SKIRT_K")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let vs_constants = [("skirt_k", skirt_k)];
        // HDR path: the color target is Rgba16Float only when HDR is on, so it signals
        // linear shading (tint/fog decode).
        let linear = if surface_format == wgpu::TextureFormat::Rgba16Float {
            1.0
        } else {
            0.0
        };
        let fs_constants = [("linear", linear)];

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_water_pipeline_layout"),
            bind_group_layouts: &[Some(camera_layout), Some(&group1_layout)],
            immediate_size: 0,
        });
        let grid_attrs = wgpu::vertex_attr_array![0 => Float32x3];
        let inst_attrs =
            wgpu::vertex_attr_array![1 => Float32x2, 2 => Float32, 3 => Uint32, 4 => Float32x2];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgr_water_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_water"),
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
                        array_stride: std::mem::size_of::<WgrWaterNode>() as u64,
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
            // Transparent-ready: reversed-Z GreaterEqual test so coastlines (drawn
            // first, nearer) occlude water, but depth-write OFF so overlapping wave
            // tiles never self-occlude — nothing 3D draws behind water in-segment.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_water"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &fs_constants,
                    ..Default::default()
                },
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Water {
            params_ubo,
            group1_bind,
            grid_vbuf,
            grid_ibuf,
            grid_index_count,
            instance_buf,
            instance_cap,
            instance_count: 0,
            pipeline,
            have_params: false,
        }
    }

    pub fn set_params(&mut self, queue: &wgpu::Queue, params: WgrWaterParams) {
        queue.write_buffer(&self.params_ubo, 0, bytemuck::bytes_of(&params));
        self.have_params = true;
    }

    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        nodes: &[WgrWaterNode],
    ) {
        self.instance_count = nodes.len() as u32;
        if nodes.is_empty() {
            return;
        }
        let needed = std::mem::size_of_val(nodes) as u64;
        if needed > self.instance_cap {
            let cap = needed.next_power_of_two();
            self.instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wgr_water_instances"),
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
    ) {
        if !self.have_params || node_count == 0 {
            return;
        }
        if first_node + node_count > self.instance_count {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, camera_bind, &[camera_offset]);
        pass.set_bind_group(1, &self.group1_bind, &[]);
        pass.set_vertex_buffer(0, self.grid_vbuf.slice(..));
        pass.set_vertex_buffer(1, self.instance_buf.slice(..));
        pass.set_index_buffer(self.grid_ibuf.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(
            0..self.grid_index_count,
            0,
            first_node..first_node + node_count,
        );
    }
}

// The reusable unit grid: (GRID_N+1)^2 vertices over [0,1]^2, two triangles per
// quad, plus a border skirt. Vertex is (u, v, skirt); the shader drops skirt
// vertices below the surface to wall off LOD-transition cracks. Identical to the
// terrain grid (kept separate so the two modules stay decoupled).
fn build_grid(device: &wgpu::Device) -> (wgpu::Buffer, wgpu::Buffer, u32) {
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
        wall(
            [(i * side) as u16, ((i + 1) * side) as u16],
            [0.0, f],
            [0.0, g],
        );
        wall(
            [(i * side + GRID_N) as u16, ((i + 1) * side + GRID_N) as u16],
            [1.0, f],
            [1.0, g],
        );
    }

    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("wgr_water_grid_vbuf"),
        contents: bytemuck::cast_slice(&verts),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("wgr_water_grid_ibuf"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    (vbuf, ibuf, indices.len() as u32)
}
