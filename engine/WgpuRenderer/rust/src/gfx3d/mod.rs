use slotmap::{Key, KeyData, SlotMap};
use wgpu::util::DeviceExt;

use crate::ffi::{WgrCamera, WgrDraw3D, WgrMat4, WgrMeshVertex, NO_PALETTE};
use crate::textures::SharedTextures;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

// The engine's bone-palette cap (MATRIX_4_ARRAY(matrix, 128)); one skinned draw
// occupies this many matrices in the palette pool and in the shader UBO.
const PALETTE_SIZE: usize = 128;

slotmap::new_key_type! {
    struct MeshKey;
}

struct Mesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    vert_count: u32,
    // Per-vertex skin data (4 bone indices + 4 weights, 8 bytes/vertex); present
    // only for skinned meshes.
    skin: Option<wgpu::Buffer>,
}

// Holds a dynamic uniform buffer + its bind group, regrown as the frame needs.
struct DynUbo {
    layout: wgpu::BindGroupLayout,
    stride: u64,
    bind_size: u64,
    buf: Option<wgpu::Buffer>,
    bind: Option<wgpu::BindGroup>,
    cap: u64,
}

impl DynUbo {
    fn new(device: &wgpu::Device, label: &str, bind_size: u64, visibility: wgpu::ShaderStages) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(bind_size),
                },
                count: None,
            }],
        });
        let align = device
            .limits()
            .min_uniform_buffer_offset_alignment
            .max(bind_size as u32) as u64;
        let stride = bind_size.div_ceil(align) * align;
        DynUbo {
            layout,
            stride,
            bind_size,
            buf: None,
            bind: None,
            cap: 0,
        }
    }

    // Ensure capacity for `count` entries; (re)create buffer + bind group on growth.
    fn ensure(&mut self, device: &wgpu::Device, count: usize) {
        let needed = count as u64 * self.stride;
        if self.cap >= needed && self.buf.is_some() {
            return;
        }
        let cap = needed.next_power_of_two().max(self.stride * 64);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgr_dyn_ubo"),
            size: cap,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgr_dyn_ubo_bind"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(self.bind_size),
                }),
            }],
        }));
        self.buf = Some(buf);
        self.cap = cap;
    }
}

pub struct Gfx3d {
    cameras: DynUbo,
    world: DynUbo,
    // Skinned draws: one PALETTE_SIZE-matrix block per slot, dynamic-offset UBO.
    palette: DynUbo,

    pipelines: [wgpu::RenderPipeline; 3],
    skinned_pipelines: [wgpu::RenderPipeline; 3],

    depth: Option<(wgpu::Texture, wgpu::TextureView)>,
    depth_size: (u32, u32),

    meshes: SlotMap<MeshKey, Mesh>,
}

impl Gfx3d {
    pub fn new(
        device: &wgpu::Device,
        textures: &SharedTextures,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_3d_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader3d.wgsl").into()),
        });
        let skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgr_3d_skinned_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader3d_skinned.wgsl").into()),
        });

        // Dynamic-offset UBOs: camera holds one entry per distinct view/proj this
        // frame, world one per draw, palette one PALETTE_SIZE-matrix block per
        // skinned draw. Camera is read by both stages (vertex: proj/view + fog
        // factor; fragment: fog_color); world/palette are vertex-only.
        let cameras = DynUbo::new(
            device,
            "wgr_3d_camera_layout",
            std::mem::size_of::<WgrCamera>() as u64,
            wgpu::ShaderStages::VERTEX_FRAGMENT,
        );
        let world = DynUbo::new(device, "wgr_3d_world_layout", 64, wgpu::ShaderStages::VERTEX);
        let palette = DynUbo::new(
            device,
            "wgr_3d_palette_layout",
            (PALETTE_SIZE * std::mem::size_of::<WgrMat4>()) as u64,
            wgpu::ShaderStages::VERTEX,
        );

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_3d_pipeline_layout"),
            bind_group_layouts: &[
                Some(&cameras.layout),
                Some(&world.layout),
                Some(&textures.texture_layout),
                Some(&textures.sampler_layout),
            ],
            immediate_size: 0,
        });
        // Skinned layout swaps the per-draw world matrix (group 1) for the bone
        // palette; groups 0/2/3 (camera/texture/sampler) are identical.
        let skinned_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgr_3d_skinned_pipeline_layout"),
            bind_group_layouts: &[
                Some(&cameras.layout),
                Some(&palette.layout),
                Some(&textures.texture_layout),
                Some(&textures.sampler_layout),
            ],
            immediate_size: 0,
        });

        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];
        let vbuf_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<WgrMeshVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attrs,
        };
        // Skin buffer: 8 bytes/vertex — Uint8x4 bone indices + Unorm8x4 weights.
        let skin_attrs = wgpu::vertex_attr_array![3 => Uint8x4, 4 => Unorm8x4];
        let skin_layout = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &skin_attrs,
        };

        let make_pipeline = |layout: &wgpu::PipelineLayout,
                             module: &wgpu::ShaderModule,
                             buffers: &[wgpu::VertexBufferLayout],
                             blend: Option<wgpu::BlendState>,
                             depth_write: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("wgr_3d_pipeline"),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(depth_write),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let alpha = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let additive = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        };
        let plain_buffers = std::slice::from_ref(&vbuf_layout);
        let skinned_buffers = [vbuf_layout.clone(), skin_layout];
        let pipelines = [
            make_pipeline(&pipeline_layout, &shader, plain_buffers, None, true),
            make_pipeline(&pipeline_layout, &shader, plain_buffers, Some(alpha), false),
            make_pipeline(&pipeline_layout, &shader, plain_buffers, Some(additive), false),
        ];
        let skinned_pipelines = [
            make_pipeline(&skinned_layout, &skinned_shader, &skinned_buffers, None, true),
            make_pipeline(&skinned_layout, &skinned_shader, &skinned_buffers, Some(alpha), false),
            make_pipeline(&skinned_layout, &skinned_shader, &skinned_buffers, Some(additive), false),
        ];

        Gfx3d {
            cameras,
            world,
            palette,
            pipelines,
            skinned_pipelines,
            depth: None,
            depth_size: (0, 0),
            meshes: SlotMap::with_key(),
        }
    }

    pub fn mesh_create(
        &mut self,
        device: &wgpu::Device,
        verts: &[WgrMeshVertex],
        indices: &[u16],
    ) -> u64 {
        if verts.is_empty() || indices.is_empty() {
            return 0;
        }
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_3d_vbuf"),
            contents: bytemuck::cast_slice(verts),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_3d_ibuf"),
            contents: bytemuck::cast_slice(indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let key = self.meshes.insert(Mesh {
            vbuf,
            ibuf,
            index_count: indices.len() as u32,
            vert_count: verts.len() as u32,
            skin: None,
        });
        key.data().as_ffi()
    }

    // Attach interleaved per-vertex skin data (4 bone indices + 4 weights).
    pub fn mesh_set_skin(&mut self, device: &wgpu::Device, handle: u64, bones: &[u8], weights: &[u8]) {
        let Some(mesh) = self.meshes.get_mut(KeyData::from_ffi(handle).into()) else {
            return;
        };
        let n = mesh.vert_count as usize;
        if bones.len() < n * 4 || weights.len() < n * 4 {
            return;
        }
        // Interleave to 8 bytes/vertex: [b0 b1 b2 b3 w0 w1 w2 w3].
        let mut data = Vec::with_capacity(n * 8);
        for v in 0..n {
            data.extend_from_slice(&bones[v * 4..v * 4 + 4]);
            data.extend_from_slice(&weights[v * 4..v * 4 + 4]);
        }
        mesh.skin = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wgr_3d_skin"),
            contents: &data,
            usage: wgpu::BufferUsages::VERTEX,
        }));
    }

    // Re-upload vertex data for an existing (dynamic) mesh, e.g. a skeletally
    // animated character whose vertices are CPU-transformed each frame. The
    // topology (indices) is unchanged; only positions/normals/uvs are rewritten.
    pub fn mesh_update(&mut self, queue: &wgpu::Queue, handle: u64, verts: &[WgrMeshVertex]) {
        let Some(mesh) = self.meshes.get(KeyData::from_ffi(handle).into()) else {
            return;
        };
        if verts.is_empty() || verts.len() as u32 > mesh.vert_count {
            return;
        }
        queue.write_buffer(&mesh.vbuf, 0, bytemuck::cast_slice(verts));
    }

    pub fn mesh_destroy(&mut self, handle: u64) {
        if handle != 0 {
            self.meshes.remove(KeyData::from_ffi(handle).into());
        }
    }

    // (Re)create the depth target to match the surface
    pub fn ensure_depth(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if self.depth_size == size && self.depth.is_some() {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_3d_depth"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth = Some((texture, view));
        self.depth_size = size;
    }

    pub fn depth_view(&self) -> Option<&wgpu::TextureView> {
        self.depth.as_ref().map(|(_, v)| v)
    }

    // Upload cameras, per-draw world matrices, and the skinned-draw bone palette;
    // regrow the dynamic UBOs. `palette` is a flat pool of PALETTE_SIZE-matrix
    // blocks, one per palette slot (world already pre-multiplied in on the C++ side).
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cameras: &[WgrCamera],
        draws: &[WgrDraw3D],
        palette: &[WgrMat4],
    ) {
        if !cameras.is_empty() {
            self.cameras.ensure(device, cameras.len());
            let buf = self.cameras.buf.as_ref().unwrap();
            for (i, c) in cameras.iter().enumerate() {
                queue.write_buffer(buf, i as u64 * self.cameras.stride, bytemuck::bytes_of(c));
            }
        }
        if !draws.is_empty() {
            self.world.ensure(device, draws.len());
            let buf = self.world.buf.as_ref().unwrap();
            for (i, d) in draws.iter().enumerate() {
                queue.write_buffer(
                    buf,
                    i as u64 * self.world.stride,
                    bytemuck::bytes_of(&d.world),
                );
            }
        }
        // One dynamic-UBO slot per PALETTE_SIZE-matrix block. A block is exactly
        // the UBO bind size, so slot s lives at s * stride.
        let slots = palette.len() / PALETTE_SIZE;
        if slots > 0 {
            self.palette.ensure(device, slots);
            let buf = self.palette.buf.as_ref().unwrap();
            for s in 0..slots {
                let block = &palette[s * PALETTE_SIZE..(s + 1) * PALETTE_SIZE];
                queue.write_buffer(
                    buf,
                    s as u64 * self.palette.stride,
                    bytemuck::cast_slice(block),
                );
            }
        }
    }

    // Issue one indexed draw. `index` is the draw's slot in the prepared arrays,
    // selecting its world matrix; `d.camera` selects its camera.
    pub fn draw_one(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        textures: &SharedTextures,
        d: &WgrDraw3D,
        index: u32,
    ) {
        if d.index_count == 0 {
            return;
        }
        let (Some(camera_bind), Some(world_bind)) =
            (self.cameras.bind.as_ref(), self.world.bind.as_ref())
        else {
            return;
        };
        let Some(mesh) = self.meshes.get(KeyData::from_ffi(d.mesh).into()) else {
            return;
        };
        if d.index_begin + d.index_count > mesh.index_count {
            return;
        }

        let camera_off = &[(d.camera as u64 * self.cameras.stride) as u32];
        let index_range = d.index_begin..(d.index_begin + d.index_count);

        // Skinned path: needs a palette slot AND the mesh to carry skin data.
        // Group 1 becomes the bone palette; groups 0/2/3 are shared.
        let skinned = d.palette_slot != NO_PALETTE;
        if let (true, Some(skin), Some(palette_bind)) =
            (skinned, mesh.skin.as_ref(), self.palette.bind.as_ref())
        {
            let pipeline = self
                .skinned_pipelines
                .get(d.blend as usize)
                .unwrap_or(&self.skinned_pipelines[0]);
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, camera_bind, camera_off);
            pass.set_bind_group(
                1,
                palette_bind,
                &[(d.palette_slot as u64 * self.palette.stride) as u32],
            );
            pass.set_bind_group(2, textures.texture_bind(d.texture_id), &[]);
            pass.set_bind_group(3, textures.sampler_bind(d.sampler.index()), &[]);
            pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
            pass.set_vertex_buffer(1, skin.slice(..));
            pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(index_range, 0, 0..1);
            return;
        }

        let pipeline = self
            .pipelines
            .get(d.blend as usize)
            .unwrap_or(&self.pipelines[0]);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, camera_bind, camera_off);
        pass.set_bind_group(1, world_bind, &[(index as u64 * self.world.stride) as u32]);
        pass.set_bind_group(2, textures.texture_bind(d.texture_id), &[]);
        pass.set_bind_group(3, textures.sampler_bind(d.sampler.index()), &[]);
        pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
        pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(index_range, 0, 0..1);
    }
}
