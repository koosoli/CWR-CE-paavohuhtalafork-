mod ffi;
mod gfx2d;
mod gfx3d;
mod handles;
mod log;
mod shaders;
mod terrain;
mod textures;

use crate::ffi::{
    WgrCamera, WgrCmd, WgrCmdKind, WgrDraw2DBatch, WgrDraw3D, WgrMat4, WgrMeshVertex,
    WgrOverlayDraw, WgrOverlayVertex, WgrLight, WgrShadowCaster, WgrShadowPass,
    WgrTerrainBatch, WgrTerrainNode, WgrTerrainParams, WgrVertex2D,
};
use crate::gfx2d::Gfx2d;
use crate::gfx3d::Gfx3d;
use crate::log::{LogSink, log_level};
use crate::terrain::Terrain;
use crate::textures::{SharedTextures, TextureData, TextureFormat};

pub struct Renderer {
    log: LogSink,
    // `'static` is sound because C++ keeps the window alive until after `wgr_destroy`.
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    textures: SharedTextures,
    gfx2d: Gfx2d,
    gfx3d: Gfx3d,
    terrain: Terrain,
}

impl Renderer {
    fn new(desc: &ffi::WgrSurfaceDesc, log: LogSink) -> Result<Self, String> {
        let (raw_display_handle, raw_window_handle) = handles::build_handles(desc)?;

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

        let surface: wgpu::Surface<'static> = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            })
        }
        .map_err(|e| format!("create_surface_unsafe failed: {e}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|e| format!("request_adapter failed: {e}"))?;

        let info = adapter.get_info();
        log.log(
            log_level::INFO,
            &format!(
                "wgpu adapter: {} ({:?}, {:?})",
                info.name, info.backend, info.device_type
            ),
        );

        let bc_features = adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
        let bc_supported = !bc_features.is_empty();
        if !bc_supported {
            log.log(
                log_level::WARN,
                "wgpu adapter lacks TEXTURE_COMPRESSION_BC; DXT textures will fail to upload",
            );
        }

        // Terrain samples its ground textures through a bindless binding_array
        // (per-layer native sizes and formats), so descriptor indexing is a hard
        // requirement. Every DX12 resource-binding-tier-2+ / Vulkan 1.2 desktop
        // GPU has it; adapters without it fall back to GL33 at the factory level.
        let bindless = wgpu::Features::TEXTURE_BINDING_ARRAY
            | wgpu::Features::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING;
        if !adapter.features().contains(bindless) {
            return Err(format!(
                "adapter lacks binding-array features required for terrain (has {:?})",
                adapter.features() & bindless
            ));
        }
        // Optional: lets the terrain bind group carry fewer views than the
        // declared array size; without it unused slots are padded with a dummy.
        let partially_bound = adapter.features() & wgpu::Features::PARTIALLY_BOUND_BINDING_ARRAY;

        let required_limits = wgpu::Limits {
            max_binding_array_elements_per_shader_stage: terrain::TERRAIN_MAX_GROUND_LAYERS,
            ..Default::default()
        };

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: bc_features | bindless | partially_bound,
            required_limits,
            ..Default::default()
        }))
        .map_err(|e| format!("request_device failed: {e}"))?;

        let mut config = surface
            .get_default_config(&adapter, desc.width.max(1), desc.height.max(1))
            .ok_or_else(|| "surface is not supported by the chosen adapter".to_string())?;

        // GL33 presents gamma-naive: the engine's already-sRGB 8-bit colors go
        // straight to the framebuffer. Render to a non-sRGB surface so
        // wgpu doesn't apply a second linear->sRGB encode on write.
        let linear = config.format.remove_srgb_suffix();
        if linear != config.format && surface.get_capabilities(&adapter).formats.contains(&linear) {
            config.format = linear;
        }

        surface.configure(&device, &config);

        let textures = SharedTextures::new(&device, &queue, bc_supported);
        // One composer, pre-loaded with the shared shader modules, shared by the
        // 3D subsystems that #import them.
        let mut composer = shaders::build_composer();
        let gfx2d = Gfx2d::new(&device, &textures, config.format);
        let gfx3d = Gfx3d::new(&device, &textures, config.format, &mut composer);
        let terrain = Terrain::new(
            &device,
            &queue,
            gfx3d.camera_layout(),
            config.format,
            !partially_bound.is_empty(),
            textures.white_view().clone(),
            &mut composer,
        );

        Ok(Self {
            log,
            surface,
            device,
            queue,
            config,
            textures,
            gfx2d,
            gfx3d,
            terrain,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    // `None` = skip this frame
    fn acquire(&mut self) -> Result<Option<wgpu::SurfaceTexture>, String> {
        use wgpu::CurrentSurfaceTexture as Cst;
        match self.surface.get_current_texture() {
            Cst::Success(t) | Cst::Suboptimal(t) => Ok(Some(t)),
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&self.device, &self.config);
                Ok(None)
            }
            Cst::Timeout | Cst::Occluded => Ok(None),
            Cst::Validation => Err("get_current_texture: validation error".to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_frame(
        &mut self,
        clear: [f32; 4],
        fog: [f32; 3],
        cameras: &[WgrCamera],
        draws3d: &[WgrDraw3D],
        verts: &[WgrVertex2D],
        batches: &[WgrDraw2DBatch],
        cmds: &[WgrCmd],
        palette: &[WgrMat4],
        shadow: &WgrShadowPass,
        shadow_casters: &[WgrShadowCaster],
        overlay_verts: &[WgrOverlayVertex],
        overlay_indices: &[u16],
        overlay_draws: &[WgrOverlayDraw],
        terrain_nodes: &[WgrTerrainNode],
        terrain_batches: &[WgrTerrainBatch],
        lights: &[WgrLight],
    ) -> Result<(), String> {
        let screen = glam::Vec2::new(self.config.width as f32, self.config.height as f32);
        self.gfx2d
            .prepare(&self.device, &self.queue, screen, fog, verts);
        self.gfx2d
            .prepare_overlay(&self.device, &self.queue, overlay_verts, overlay_indices);
        self.gfx3d
            .ensure_depth(&self.device, self.config.width, self.config.height);
        // Shadows first: prepare() binds the frame's final shadow target into
        // the camera group.
        self.gfx3d
            .prepare_shadows(&self.device, &self.queue, shadow, shadow_casters);
        // The terrain owns the sun-shadow mask; lend its view + world->UV mapping to
        // the shared camera group(0) so lit meshes receive terrain shadow too.
        let shadow_mask_view = self.terrain.shadow_mask_view();
        let shadow_mask_gen = self.terrain.shadow_gen();
        let shadow_mapping = self.terrain.shadow_mapping();
        self.gfx3d.prepare(
            &self.device,
            &self.queue,
            cameras,
            draws3d,
            palette,
            lights,
            &shadow_mask_view,
            shadow_mask_gen,
            &shadow_mapping,
        );
        self.terrain
            .prepare(&self.device, &self.queue, terrain_nodes);

        let Some(frame) = self.acquire()? else {
            return Ok(());
        };

        let color = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let depth = self
            .gfx3d
            .depth_view()
            .ok_or("depth target missing")?
            .clone();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wgr_frame"),
            });

        // Cascade shadow depth passes run first so every segment's draws can
        // sample the completed map, regardless of submission order.
        self.gfx3d
            .render_shadow_passes(&mut encoder, &self.textures, shadow, shadow_casters);

        // Amortized terrain sun-shadow sweep (long-range heightfield self-shadow),
        // recorded before the render segments sample its mask. The sun direction is
        // uniform across cameras; sun_dir_world is the light travel direction, so
        // negate it for the surface-to-light march.
        if let Some(cam) = cameras.first() {
            let s = cam.sun_dir_world;
            let sun_to_light = glam::Vec3::new(-s[0], -s[1], -s[2]);
            self.terrain
                .render_shadow_mask(&self.queue, &mut encoder, sun_to_light);
        }

        // Replay the command stream as one or more segments split at CLEAR_DEPTH.
        // The first segment clears colour; every segment clears depth. Within a
        // segment, 2D and 3D draws render interleaved in submission order.
        let mut first = true;
        let mut start = 0usize;
        loop {
            let end = cmds[start..]
                .iter()
                .position(|c| c.kind == WgrCmdKind::ClearDepth as u32)
                .map(|p| start + p)
                .unwrap_or(cmds.len());

            let color_load = if first {
                wgpu::LoadOp::Clear(wgpu::Color {
                    r: clear[0] as f64,
                    g: clear[1] as f64,
                    b: clear[2] as f64,
                    a: clear[3] as f64,
                })
            } else {
                wgpu::LoadOp::Load
            };
            first = false;

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_segment"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: color_load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        // Reversed-Z: far plane is 0
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    // Cleared to 0 each segment so shadow draws (stencil EQUAL 0 /
                    // INCR) darken each pixel at most once.
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            for cmd in &cmds[start..end] {
                if cmd.kind == WgrCmdKind::Draw2D as u32 {
                    if let Some(b) = batches.get(cmd.arg as usize) {
                        self.gfx2d.draw_one(&mut pass, &self.textures, b);
                    }
                } else if cmd.kind == WgrCmdKind::Draw3D as u32 {
                    if let Some(d) = draws3d.get(cmd.arg as usize) {
                        self.gfx3d.draw_one(&mut pass, &self.textures, d, cmd.arg);
                    }
                } else if cmd.kind == WgrCmdKind::DrawTerrain as u32 {
                    if let (Some(b), Some(cam)) = (
                        terrain_batches.get(cmd.arg as usize),
                        self.gfx3d.camera_bind(),
                    ) {
                        let off = (b.camera as u64 * self.gfx3d.camera_stride()) as u32;
                        self.terrain
                            .draw(&mut pass, cam, off, b.first_node, b.node_count);
                    }
                }
            }
            drop(pass);

            if end >= cmds.len() {
                break;
            }
            start = end + 1;
        }

        // Dev-panel overlay composites over the finished frame, no depth.
        if !overlay_draws.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.gfx2d.render_overlay(
                &mut pass,
                &self.textures,
                overlay_draws,
                self.config.width,
                self.config.height,
            );
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    fn texture_create(
        &mut self,
        width: u32,
        height: u32,
        format: TextureFormat,
        mip_count: u32,
        gen_mips: bool,
        data: &[u8],
    ) -> u64 {
        self.textures.create(
            &self.device,
            &self.queue,
            &TextureData {
                width,
                height,
                format,
                mip_count,
                gen_mips,
                bytes: data,
            },
        )
    }

    fn texture_update(&mut self, handle: u64, data: &[u8]) {
        self.textures.update_rgba(&self.queue, handle, data);
    }

    fn texture_destroy(&mut self, handle: u64) {
        self.textures.destroy(handle);
    }

    fn mesh_create(&mut self, verts: &[WgrMeshVertex], indices: &[u16]) -> u64 {
        self.gfx3d.mesh_create(&self.device, verts, indices)
    }

    fn mesh_update(&mut self, handle: u64, verts: &[WgrMeshVertex]) {
        self.gfx3d.mesh_update(&self.queue, handle, verts);
    }

    fn mesh_set_skin(&mut self, handle: u64, bones: &[u8], weights: &[u8]) {
        self.gfx3d
            .mesh_set_skin(&self.device, handle, bones, weights);
    }

    fn mesh_destroy(&mut self, handle: u64) {
        self.gfx3d.mesh_destroy(handle);
    }

    fn terrain_set_heightmap(&mut self, heights: &[f32], params: WgrTerrainParams) {
        self.terrain
            .set_heightmap(&self.device, &self.queue, heights, params);
    }

    fn terrain_set_ground_layers(&mut self, handles: &[u64]) {
        let views: Vec<wgpu::TextureView> = handles
            .iter()
            .map(|&h| self.textures.texture_view(h).clone())
            .collect();
        self.terrain.set_ground_layers(&self.device, views);
    }

    fn terrain_set_index_map(&mut self, width: u32, height: u32, indices: &[u16]) {
        self.terrain
            .set_index_map(&self.device, &self.queue, width, height, indices);
    }

    fn terrain_set_jitter_map(&mut self, width: u32, height: u32, offsets: &[i8]) {
        self.terrain
            .set_jitter_map(&self.device, &self.queue, width, height, offsets);
    }

    fn terrain_set_sun_shadow(&mut self, strength: f32, scale: u32, max_steps: u32, penumbra_deg: f32) {
        self.terrain
            .set_sun_shadow_params(&self.device, strength, scale, max_steps, penumbra_deg);
    }

    fn terrain_set_detail_layer(&mut self, handle: u64) {
        if handle == 0 {
            return;
        }
        let view = self.textures.texture_view(handle).clone();
        self.terrain.set_detail_layer(&self.device, view);
    }

    fn shadow_map_read(&mut self, layer: u32, out: &mut [f32]) -> u32 {
        self.gfx3d
            .shadow_map_read(&self.device, &self.queue, layer, out)
    }

    fn shadow_depth_probe(
        &mut self,
        light_vp: &[f32; 16],
        verts_xyz: &[f32],
        res: u32,
        out: &mut [f32],
    ) -> bool {
        self.gfx3d.shadow_depth_probe(
            &self.device,
            &self.queue,
            &self.textures,
            light_vp,
            verts_xyz,
            res,
            out,
        )
    }
}
