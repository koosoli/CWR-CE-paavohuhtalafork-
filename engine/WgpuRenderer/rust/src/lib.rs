mod ffi;
mod gfx2d;
mod gfx3d;
mod handles;
mod log;
mod shaders;
mod terrain;
mod textures;
mod tonemap;

use crate::ffi::{
    WgrCamera, WgrCmd, WgrDraw2DBatch, WgrDraw3D, WgrMat4, WgrMeshVertex,
    WgrOverlayDraw, WgrOverlayVertex, WgrLight, WgrShadowCaster, WgrShadowPass,
    WgrTerrainBatch, WgrTerrainNode, WgrTerrainParams, WgrVertex2D,
};
use crate::gfx2d::Gfx2d;
use crate::gfx3d::{Gfx3d, env_f32};
use crate::log::{LogSink, log_level};
use crate::terrain::Terrain;
use crate::textures::{SharedTextures, TextureData, TextureFormat};
use crate::tonemap::Tonemap;

// Offscreen HDR scene target format (see docs/hdr-pipeline-plan.md §0.2). Alpha kept
// for blending; full float precision to avoid banding in dark skies at night.
const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

// Like env_f32 but keeps a 0 value (env_f32 filters to >0 for scales). Used for the
// tonemap mode/encode toggles where 0 is a meaningful "off".
fn env_f32_opt(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(default)
}

// sRGB -> linear for a single channel (matches the shader `srgb_to_linear`), for
// linearizing the CPU-side clear colour that seeds the HDR target.
fn srgb_to_linear_ch(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

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
    // HDR pipeline (docs/hdr-pipeline-plan.md). When enabled, the 3D/terrain/2D
    // scene renders into `hdr` (linear once Stage 2 lands) and `tonemap` resolves it
    // to the swapchain; the dev overlay + (later) screen-space UI composite after.
    // All None/false = the LDR-direct-to-swapchain path, the A/B reference.
    hdr_enabled: bool,
    hdr: Option<(wgpu::Texture, wgpu::TextureView)>,
    hdr_size: (u32, u32),
    tonemap: Option<Tonemap>,
    // Live tonemap/look params, pushed from the ImGui Tonemap tab (wgr_set_tonemap).
    // Seeded from WGR_* env for continuity; the tab is the source of truth once open.
    tonemap_params: ffi::WgrTonemap,
}

impl Renderer {
    fn new(desc: &ffi::WgrSurfaceDesc, log: LogSink) -> Result<Self, String> {
        let (raw_display_handle, raw_window_handle) = handles::build_handles(desc)?;

        // wgpu only enables VK_EXT_debug_utils — and thus emits our
        // push_debug_group markers + buffer/texture labels into a RenderDoc capture
        // — when the DEBUG instance flag is set. InstanceFlags::from_build_config()
        // (which the _from_env constructor seeds from) clears DEBUG in optimized
        // rwdi/release builds, so a profiling build shows an unlabelled, ungrouped
        // command stream. Force DEBUG on so captures stay legible; opt into the
        // validation layers separately via WGR_GPU_VALIDATION (they are expensive).
        let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        instance_desc.flags |= wgpu::InstanceFlags::DEBUG;
        if std::env::var("WGR_GPU_VALIDATION").is_ok() {
            instance_desc.flags |= wgpu::InstanceFlags::VALIDATION;
        }
        log.log(
            log_level::INFO,
            &format!("wgpu instance flags: {:?}", instance_desc.flags),
        );
        let instance = wgpu::Instance::new(instance_desc);

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
            // The lit mesh pipelines take a 5th bind group (group 4) for the terrain
            // heightmap used to conform vegetation on the GPU. Ample on desktop.
            max_bind_groups: 5,
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

        // HDR path (docs/hdr-pipeline-plan.md). Gated by WGR_HDR for now; the engine
        // config/CLI flag drives it once Stage 2's C++ work lands. When on, the scene
        // subsystems target the offscreen HDR format and a tonemap pass resolves to the
        // swapchain; the overlay pipeline always targets the swapchain format.
        let hdr_enabled = std::env::var("WGR_HDR").map(|v| v != "0").unwrap_or(false);
        let color_format = if hdr_enabled { HDR_FORMAT } else { config.format };
        if hdr_enabled {
            log.log(log_level::INFO, "wgpu HDR path enabled (WGR_HDR)");
        }

        let textures = SharedTextures::new(&device, &queue, bc_supported);
        // One composer, pre-loaded with the shared shader modules, shared by the
        // 3D subsystems that #import them.
        let mut composer = shaders::build_composer();
        let gfx2d = Gfx2d::new(&device, &textures, color_format, config.format);
        let gfx3d = Gfx3d::new(&device, &textures, color_format, &mut composer);
        let terrain = Terrain::new(
            &device,
            &queue,
            gfx3d.camera_layout(),
            color_format,
            !partially_bound.is_empty(),
            textures.white_view().clone(),
            &mut composer,
        );
        let tonemap = hdr_enabled.then(|| Tonemap::new(&device, config.format));
        // Seed live params from the env knobs so behaviour is unchanged until the
        // ImGui tab pushes its own values (env_f32's >0 filter is fine for scales;
        // env_f32_opt keeps a 0 for the mode/encode toggles).
        let tonemap_params = ffi::WgrTonemap {
            exposure: env_f32("WGR_EXPOSURE", 1.0),
            mode: env_f32_opt("WGR_TONEMAP", 1.0),
            encode: env_f32_opt("WGR_HDR_ENCODE", 1.0),
            ..Default::default()
        };

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
            hdr_enabled,
            hdr: None,
            hdr_size: (0, 0),
            tonemap,
            tonemap_params,
        })
    }

    // Live update from the ImGui Tonemap tab (via wgr_set_tonemap). Applied on the
    // next frame's resolve; ignored on the LDR-direct path (no tonemap pass).
    fn set_tonemap(&mut self, params: ffi::WgrTonemap) {
        self.tonemap_params = params;
    }

    // Tonemap the HDR scene target onto `dst` (the swapchain). No-op if the tonemap
    // pass doesn't exist (LDR-direct path).
    fn run_tonemap(&self, encoder: &mut wgpu::CommandEncoder, dst: &wgpu::TextureView) {
        let Some(tonemap) = self.tonemap.as_ref() else {
            return;
        };
        // Live params from the ImGui Tonemap tab (seeded from WGR_* at startup).
        tonemap.upload_params(&self.queue, &self.tonemap_params);
        encoder.push_debug_group("wgr_tonemap");
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wgr_tonemap"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
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
        tonemap.render(&mut pass);
        drop(pass);
        encoder.pop_debug_group();
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    // (Re)allocate the offscreen HDR scene target to match the swapchain size, and
    // repoint the tonemap resolve at the new view. No-op when the HDR path is off or
    // the size is unchanged. Mirrors Gfx3d::ensure_depth.
    fn ensure_hdr(&mut self, width: u32, height: u32) {
        if !self.hdr_enabled || width == 0 || height == 0 {
            return;
        }
        if self.hdr.is_some() && self.hdr_size == (width, height) {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_hdr_target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        if let Some(tonemap) = self.tonemap.as_mut() {
            tonemap.set_source(&self.device, &view);
        }
        self.hdr = Some((texture, view));
        self.hdr_size = (width, height);
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
        // Lend the terrain heightmap to the mesh conform group (group 4) so object
        // vertex shaders conform ClipLand vegetation to SurfaceY without CPU rewrites.
        let heightmap_view = self.terrain.heightmap_view();
        let heightmap_gen = self.terrain.heightmap_gen();
        let conform_params = self.terrain.conform_params();
        // Bucket the frame's 3D draws into instanced groups (see Gfx3d::plan_3d). The
        // plan's `order` drives the storage-array pack order in prepare(); its `ops`
        // replace the raw command stream in the replay loop below.
        let plan = self.gfx3d.plan_3d(cmds, draws3d);
        self.gfx3d.prepare(
            &self.device,
            &self.queue,
            cameras,
            draws3d,
            &plan.order,
            palette,
            lights,
            &shadow_mask_view,
            shadow_mask_gen,
            &shadow_mapping,
            &heightmap_view,
            heightmap_gen,
            &conform_params,
        );
        self.terrain
            .prepare(&self.device, &self.queue, terrain_nodes);
        self.ensure_hdr(self.config.width, self.config.height);

        let Some(frame) = self.acquire()? else {
            return Ok(());
        };

        let color = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Scene (3D/terrain/interleaved 2D) renders here: the HDR target when the HDR
        // path is on, else straight to the swapchain. TextureView clones are cheap
        // (Arc), so cloning avoids holding a borrow of self across the segment loop.
        let scene_view = self
            .hdr
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| color.clone());
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
        // sample the completed map, regardless of submission order. Debug groups
        // (here and below) name each phase in a RenderDoc capture.
        encoder.push_debug_group("wgr_shadow_cascades");
        self.gfx3d
            .render_shadow_passes(&mut encoder, &self.textures, shadow, shadow_casters);
        encoder.pop_debug_group();

        // Amortized terrain sun-shadow sweep (long-range heightfield self-shadow),
        // recorded before the render segments sample its mask. The sun direction is
        // uniform across cameras; sun_dir_world is the light travel direction, so
        // negate it for the surface-to-light march.
        if let Some(cam) = cameras.first() {
            let s = cam.sun_dir_world;
            let sun_to_light = glam::Vec3::new(-s[0], -s[1], -s[2]);
            // The mask sweep is amortized (recorded only when the heightmap changes
            // or the sun moves), so its debug group is pushed inside — wrapping it
            // here would leave an empty group on the frames it skips.
            self.terrain
                .render_shadow_mask(&self.queue, &mut encoder, sun_to_light);
        }

        // Replay the instancing plan. It splits into a scene phase and a UI phase at
        // the Resolve op (the engine's scene->UI seam, WGR_CMD_RESOLVE): scene draws
        // render into the HDR target; the tonemap then resolves it to the swapchain;
        // the UI phase draws display-referred straight to the swapchain (2D uses the
        // swapchain-format pipeline set). Segments additionally split at ClearDepth
        // (each clears depth). On the LDR-direct path there is no HDR target/tonemap,
        // and Resolve is a no-op (scene_view IS the swapchain throughout).
        use crate::gfx3d::Plan3dOp;
        let ops = &plan.ops;

        // The clear colour seeds the scene target. On the HDR path that target is
        // linear, so decode the gamma-space clear the engine supplies.
        let clear_rgb = if self.hdr_enabled {
            [
                srgb_to_linear_ch(clear[0]),
                srgb_to_linear_ch(clear[1]),
                srgb_to_linear_ch(clear[2]),
            ]
        } else {
            [clear[0], clear[1], clear[2]]
        };

        // `target` is where the current phase's segments render (HDR then swapchain);
        // `display_2d` picks the swapchain-format 2D pipelines in the UI phase.
        let mut target = scene_view.clone();
        let mut display_2d = false;
        let mut clear_color_next = true;
        let mut resolved = false;
        let mut start = 0usize;
        let mut seg_idx = 0usize;
        loop {
            let end = ops[start..]
                .iter()
                .position(|o| matches!(o, Plan3dOp::ClearDepth | Plan3dOp::Resolve))
                .map(|p| start + p)
                .unwrap_or(ops.len());

            let color_load = if clear_color_next {
                wgpu::LoadOp::Clear(wgpu::Color {
                    r: clear_rgb[0] as f64,
                    g: clear_rgb[1] as f64,
                    b: clear_rgb[2] as f64,
                    a: clear[3] as f64,
                })
            } else {
                wgpu::LoadOp::Load
            };
            clear_color_next = false;

            let seg_label = format!("wgr_segment_{seg_idx}");
            seg_idx += 1;
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(&seg_label),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target,
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

                // Tracks bind/pipeline state across consecutive 3D draws so draw_one can
                // skip redundant re-binds. A terrain or 2D draw sets its own pipeline +
                // groups, invalidating the 3D state, so reset it whenever one runs.
                let mut st3d = crate::gfx3d::Pass3dState::default();
                for op in &ops[start..end] {
                    match op {
                        Plan3dOp::Draw2D(arg) => {
                            st3d = crate::gfx3d::Pass3dState::default();
                            if let Some(b) = batches.get(*arg as usize) {
                                self.gfx2d.draw_one(&mut pass, &self.textures, b, display_2d);
                            }
                        }
                        Plan3dOp::Draw3D { draw, base, count } => {
                            if let Some(d) = draws3d.get(*draw as usize) {
                                self.gfx3d.draw_one(
                                    &mut pass,
                                    &self.textures,
                                    d,
                                    *base,
                                    *count,
                                    &mut st3d,
                                );
                            }
                        }
                        Plan3dOp::Terrain(arg) => {
                            st3d = crate::gfx3d::Pass3dState::default();
                            if let (Some(b), Some(cam)) = (
                                terrain_batches.get(*arg as usize),
                                self.gfx3d.camera_bind(),
                            ) {
                                let off = (b.camera as u64 * self.gfx3d.camera_stride()) as u32;
                                self.terrain
                                    .draw(&mut pass, cam, off, b.first_node, b.node_count);
                            }
                        }
                        Plan3dOp::ClearDepth | Plan3dOp::Resolve => {}
                    }
                }
                drop(pass);
            }

            if end >= ops.len() {
                break;
            }
            // Scene->UI seam: resolve the HDR scene and switch to display-referred UI.
            if matches!(ops[end], Plan3dOp::Resolve) && self.tonemap.is_some() && !resolved {
                self.run_tonemap(&mut encoder, &color);
                resolved = true;
                target = color.clone();
                display_2d = true;
                clear_color_next = false; // UI loads the tonemapped scene
            }
            start = end + 1;
        }

        // Fallback: an HDR frame that never emitted the Resolve marker still needs
        // resolving so the scene reaches the swapchain.
        if self.tonemap.is_some() && !resolved {
            self.run_tonemap(&mut encoder, &color);
        }

        // Dev-panel overlay composites over the finished frame, no depth.
        if !overlay_draws.is_empty() {
            encoder.push_debug_group("wgr_overlay");
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
            drop(pass);
            encoder.pop_debug_group();
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
