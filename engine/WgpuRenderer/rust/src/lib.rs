mod ffi;
mod gfx2d;
mod gfx3d;
mod handles;
mod log;
mod shaders;
mod sky;
mod terrain;
mod textures;
mod water;
mod bloom;
mod exposure;
mod tonemap;

use crate::ffi::{
    WgrCamera, WgrCmd, WgrDraw2DBatch, WgrDraw3D, WgrInstance, WgrMat4, WgrMeshVertex,
    WgrModelLod, WgrModelMaterial, WgrModelSection, WgrOverlayDraw, WgrOverlayVertex, WgrLight,
    WgrShadowCaster, WgrShadowPass, WgrTerrainBatch, WgrTerrainNode, WgrTerrainParams,
    WgrVertex2D, WgrWaterBatch, WgrWaterNode, WgrWaterParams,
};
use crate::gfx2d::Gfx2d;
use crate::gfx3d::{Gfx3d, env_f32};
use crate::log::{LogSink, log_level};
use crate::sky::Sky;
use crate::terrain::Terrain;
use crate::textures::{SharedTextures, TextureData, TextureFormat};
use crate::water::Water;
use crate::bloom::Bloom;
use crate::exposure::Exposure;
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
    water: Water,
    // HDR pipeline (docs/hdr-pipeline-plan.md). When enabled, the 3D/terrain/2D
    // scene renders into `hdr` (linear once Stage 2 lands) and `tonemap` resolves it
    // to the swapchain; the dev overlay + (later) screen-space UI composite after.
    // All None/false = the LDR-direct-to-swapchain path, the A/B reference.
    hdr_enabled: bool,
    hdr: Option<(wgpu::Texture, wgpu::TextureView)>,
    // Single-sample resolve target for the MSAA scene colour (Some only when sample_count > 1).
    // The scene renders into the multisampled `hdr`; a resolve writes this, and the tonemap /
    // bloom / exposure sample it. At 1x this is None and those read `hdr` directly.
    hdr_resolve: Option<(wgpu::Texture, wgpu::TextureView)>,
    hdr_size: (u32, u32),
    // MSAA sample count of the scene targets (1 = off). Fixed at startup (WGR_MSAA); pipelines
    // and offscreen targets are built against it.
    sample_count: u32,
    tonemap: Option<Tonemap>,
    // Bloom pyramid, built alongside the tonemap on the HDR path; the resolve adds it.
    bloom: Option<Bloom>,
    // Eye adaptation / auto-exposure; produces a 1x1 exposure scale the resolve applies.
    exposure: Option<Exposure>,
    exposure_params: ffi::WgrExposure,
    // Live tonemap/look params, pushed from the ImGui Tonemap tab (wgr_set_tonemap).
    // Seeded from WGR_* env for continuity; the tab is the source of truth once open.
    tonemap_params: ffi::WgrTonemap,
    // Procedural sky (docs/procedural-sky-plan.md): a fullscreen atmospheric pass
    // drawn into the scene target before geometry. Params pushed via wgr_set_sky
    // (celestial per frame, authored on edit); skipped when control.x (enabled) = 0.
    sky: Sky,
    sky_params: ffi::WgrSky,
    // WGR_SKY_DEBUG: log the sky's camera count + chosen index when they change, to
    // catch frame-to-frame camera alternation (the suspected sun/haze stutter cause).
    sky_debug: bool,
    sky_dbg_last: (usize, usize),
    // Depth+normal prepass (docs/depth-prepass-plan.md). Ships unconditionally on wgpu
    // (decision 8); WGR_PREPASS=0 is a TEMPORARY dev A/B for bring-up validation only,
    // not a shipped runtime flag. When on, the first (world) depth segment gets a
    // depth+normal prepass and its opaque colour draws early-Z with depth-write off.
    prepass_enabled: bool,
    // Per-frame gate for the retained GPU-driven world set (objects + their prepass).
    // The set is GPU-resident and would otherwise draw every frame regardless of the
    // per-frame 3D lists; C++ raises this (wgr_set_suppress_world_objects) while the
    // world must not be shown (mission editor, loading, shutdown) so the sides letterbox
    // to black instead of leaking clutter. Set explicitly by C++ each frame.
    suppress_world_objects: bool,
    // Debug: draw the GPU-driven frustum-cull spheres (ImGui Culling tab). Off by default.
    cull_debug_draw: bool,
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
        let partially_bound_enabled = !partially_bound.is_empty();

        // GPU-driven indirect draw (docs/gpu-culling-and-depth-plan.md Stage 2). Our
        // instancing model puts each bucket's base_instance in the indirect args'
        // first_instance, so INDIRECT_FIRST_INSTANCE is the gating feature. Stage 2 issues
        // single draw_indexed_indirect (core wgpu, portable incl. Metal), so nothing more
        // is needed here; the Stage-3 GPU-produced multi-draw will request its own feature.
        // Adapter-gated exactly like `partially_bound`; when absent the direct path stays.
        let indirect_avail = adapter.features() & wgpu::Features::INDIRECT_FIRST_INSTANCE;
        let indirect_first_instance = !indirect_avail.is_empty();

        // MULTI_DRAW_INDIRECT_COUNT (docs/gpu-culling-and-depth-plan.md Stage 3b-4): a GPU
        // count buffer that trims the empty tail of the compute-produced indirect args, so the
        // GPU-driven draw dispatches only the surviving sub-draws instead of the full
        // conservative per-variant capacity. Present on desktop Vulkan/DX12; Metal lacks it and
        // falls back to the no-op-tail multi_draw. Adapter-gated like the features above.
        let mdic_avail = adapter.features() & wgpu::Features::MULTI_DRAW_INDIRECT_COUNT;
        let multi_draw_count = !mdic_avail.is_empty();

        // Bindless object textures (docs/bindless-textures-plan.md): one binding_array
        // covering all live object textures. Cap chosen so a non-PARTIALLY_BOUND adapter
        // doesn't pad an enormous array (a level with more unique textures overflows to
        // the white slot — 8192 comfortably covers OFP content). Must be >= terrain's 512.
        let object_texture_cap = 8192u32.max(terrain::TERRAIN_MAX_GROUND_LAYERS);

        // BOTH binding-array limits DEFAULT TO 0 even on devices that fully support
        // binding arrays, so both must be requested explicitly or layout creation panics
        // ("limit is 0"). Deriving from adapter.limits() is unreliable (it can report the
        // 0 default); the wgpu docs guarantee any array-capable device supports >= 500k
        // resources / 1000 samplers, and we gate on array features above, so request the
        // fixed values we use. NB wgpu counts the sampler array's 8 elements against the
        // GENERAL elements limit too (not only the sampler limit), so the object pipeline
        // layout needs `object_texture_cap + 8`; request headroom above that.
        let required_limits = wgpu::Limits {
            max_binding_array_elements_per_shader_stage: object_texture_cap + 64,
            max_binding_array_sampler_elements_per_shader_stage: 8,
            // The lit mesh pipelines take a 5th bind group (group 4) for the terrain
            // heightmap used to conform vegetation on the GPU. Ample on desktop.
            max_bind_groups: 5,
            ..Default::default()
        };

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: bc_features
                | bindless
                | partially_bound
                | indirect_avail
                | mdic_avail,
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

        // HDR path (docs/hdr-pipeline-plan.md). Now the default for the wgpu backend —
        // the procedural sky, aerial fog, sky-based lighting and tonemap/bloom/exposure
        // all live on this path, so running without it drops the renderer onto the legacy
        // gamma-naive fallback and looks broken. WGR_HDR=0 still forces it off for A/B.
        // When on, the scene subsystems target the offscreen HDR format and a tonemap pass
        // resolves to the swapchain; the overlay pipeline always targets the swapchain format.
        let prepass_enabled = std::env::var("WGR_PREPASS").map(|v| v != "0").unwrap_or(true);
        // Compute skin bake is OPT-IN (default off): it is correct + validated but pure
        // overhead until GPU-driven rendering consumes the baked rigid geometry (VS skinning
        // is ~free for OFP's low-poly characters, so amortizing it saves nothing measurable).
        // WGR_SKIN_BAKE=1 re-enables it so the path stays exercisable. See
        // docs/compute-skin-bake-plan.md + docs/gpu-culling-and-depth-plan.md.
        let skin_bake_enabled = std::env::var("WGR_SKIN_BAKE").map(|v| v != "0").unwrap_or(false);
        // Indirect draw is default-on when the adapter supports it; WGR_INDIRECT=0 forces
        // the direct draw_one path for A/B. Disabled outright without INDIRECT_FIRST_INSTANCE.
        let indirect_enabled = indirect_first_instance
            && std::env::var("WGR_INDIRECT").map(|v| v != "0").unwrap_or(true);
        // GPU-driven rendering (docs/gpu-culling-and-depth-plan.md Stage 3). Default-on now
        // that the path is built up; inert until C++ registers a retained scene (Stage 3b-3),
        // and needs first_instance for its indirect args. WGR_GPU_DRIVEN=0 forces it off.
        let gpu_driven_enabled = indirect_first_instance
            && std::env::var("WGR_GPU_DRIVEN").map(|v| v != "0").unwrap_or(true);
        let hdr_enabled = std::env::var("WGR_HDR").map(|v| v != "0").unwrap_or(true);
        let color_format = if hdr_enabled { HDR_FORMAT } else { config.format };
        // MSAA (WGR_MSAA, default 4x). Requires the HDR path: the multisampled scene colour is
        // resolved to a single-sample HDR target the tonemap samples, and WebGPU has no depth
        // resolve_target, so the LDR-direct-to-swapchain path stays 1x. Clamped to what the
        // adapter supports for every multisampled scene format (colour + depth + normal G-buffer).
        let msaa_req = std::env::var("WGR_MSAA")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(4);
        let sample_count = if hdr_enabled && msaa_req > 1 {
            let ok = [color_format, gfx3d::DEPTH_FORMAT, gfx3d::NORMAL_FORMAT]
                .iter()
                .all(|&f| {
                    adapter
                        .get_texture_format_features(f)
                        .flags
                        .sample_count_supported(msaa_req)
                });
            if ok {
                msaa_req
            } else {
                log.log(
                    log_level::WARN,
                    &format!("wgpu MSAA {msaa_req}x unsupported for the scene formats; using 1x"),
                );
                1
            }
        } else {
            1
        };
        if sample_count > 1 {
            log.log(
                log_level::INFO,
                &format!("wgpu MSAA enabled: {sample_count}x (WGR_MSAA)"),
            );
        }
        if hdr_enabled {
            log.log(log_level::INFO, "wgpu HDR path enabled (WGR_HDR)");
        }
        if !prepass_enabled {
            log.log(log_level::INFO, "wgpu depth prepass disabled (WGR_PREPASS=0)");
        }
        if skin_bake_enabled {
            log.log(
                log_level::INFO,
                "wgpu compute skin bake enabled (WGR_SKIN_BAKE); VS skinning path bypassed",
            );
        }
        if indirect_enabled {
            log.log(
                log_level::INFO,
                "wgpu GPU-driven indirect draws enabled (WGR_INDIRECT)",
            );
        } else if !indirect_first_instance {
            log.log(
                log_level::WARN,
                "adapter lacks INDIRECT_FIRST_INSTANCE; indirect draws off, using direct path",
            );
        }

        let textures = SharedTextures::new(
            &device,
            &queue,
            bc_supported,
            object_texture_cap,
            partially_bound_enabled,
        );
        // One composer, pre-loaded with the shared shader modules, shared by the
        // 3D subsystems that #import them.
        let mut composer = shaders::build_composer();
        let gfx2d = Gfx2d::new(&device, &textures, color_format, config.format, sample_count);
        let gfx3d = Gfx3d::new(
            &device,
            &textures,
            color_format,
            sample_count,
            &mut composer,
            skin_bake_enabled,
            indirect_enabled,
            gpu_driven_enabled,
            gpu_driven_enabled && multi_draw_count,
        );
        if gpu_driven_enabled {
            log.log(
                log_level::INFO,
                "wgpu GPU-driven rendering enabled (WGR_GPU_DRIVEN); inert until a scene registers",
            );
            if !multi_draw_count {
                log.log(
                    log_level::INFO,
                    "adapter lacks MULTI_DRAW_INDIRECT_COUNT; GPU-driven draws use the conservative no-op-tail path",
                );
            }
        }
        let terrain = Terrain::new(
            &device,
            &queue,
            gfx3d.camera_layout(),
            color_format,
            sample_count,
            !partially_bound.is_empty(),
            textures.white_view().clone(),
            &mut composer,
        );
        let water = Water::new(
            &device,
            &queue,
            gfx3d.camera_layout(),
            color_format,
            sample_count,
            &mut composer,
        );
        let tonemap = hdr_enabled.then(|| Tonemap::new(&device, config.format));
        let bloom = hdr_enabled.then(|| Bloom::new(&device, HDR_FORMAT));
        let exposure = hdr_enabled.then(|| Exposure::new(&device, &queue));
        // The sky targets the scene color format (HDR target or swapchain), matching
        // the scene pipelines, and self-tonemaps when that is an LDR-direct swapchain.
        let sky = Sky::new(&device, color_format, sample_count);
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
            water,
            hdr_enabled,
            hdr: None,
            hdr_resolve: None,
            hdr_size: (0, 0),
            sample_count,
            tonemap,
            bloom,
            exposure,
            exposure_params: ffi::WgrExposure::default(),
            tonemap_params,
            sky,
            sky_params: ffi::WgrSky::default(),
            sky_debug: std::env::var("WGR_SKY_DEBUG").is_ok(),
            sky_dbg_last: (usize::MAX, usize::MAX),
            prepass_enabled,
            suppress_world_objects: false,
            cull_debug_draw: false,
        })
    }

    // Live update from the ImGui Sky tab / per-frame celestial push (wgr_set_sky).
    // Applied on the next frame's sky pass.
    fn set_sky(&mut self, params: ffi::WgrSky) {
        self.sky_params = params;
    }

    // Live update from the ImGui Tonemap tab (via wgr_set_tonemap). Applied on the
    // next frame's resolve; ignored on the LDR-direct path (no tonemap pass).
    fn set_tonemap(&mut self, params: ffi::WgrTonemap) {
        self.tonemap_params = params;
    }

    // Live update from the ImGui Tonemap tab (via wgr_set_exposure). Applied next frame.
    fn set_exposure(&mut self, params: ffi::WgrExposure) {
        self.exposure_params = params;
    }

    // Debug readback of the current auto-exposure scale (blocking; dev panel only).
    fn exposure_scale(&self) -> f32 {
        self.exposure
            .as_ref()
            .map(|e| e.read_scale(&self.device, &self.queue))
            .unwrap_or(1.0)
    }

    // Tonemap the HDR scene target onto `dst` (the swapchain). No-op if the tonemap
    // pass doesn't exist (LDR-direct path).
    fn run_tonemap(&self, encoder: &mut wgpu::CommandEncoder, dst: &wgpu::TextureView) {
        let Some(tonemap) = self.tonemap.as_ref() else {
            return;
        };
        // MSAA: resolve the multisampled scene colour into the single-sample HDR target the
        // post-processing chain (bloom/exposure/tonemap) samples. An empty load/store pass with a
        // resolve_target performs the resolve at pass end; StoreOp::Discard drops the now-unneeded
        // multisampled contents. No-op at 1x (hdr_resolve is None, the chain reads `hdr` directly).
        if let (Some((_, msaa_view)), Some((_, resolve_view))) =
            (self.hdr.as_ref(), self.hdr_resolve.as_ref())
        {
            encoder.push_debug_group("wgr_hdr_resolve");
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_hdr_resolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view,
                    depth_slice: None,
                    resolve_target: Some(resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            drop(pass);
            encoder.pop_debug_group();
        }
        // Live params from the ImGui Tonemap tab (seeded from WGR_* at startup).
        tonemap.upload_params(&self.queue, &self.tonemap_params);
        // Build the bloom pyramid from the finished HDR scene (already includes aerial
        // perspective) so the resolve can add it. Skipped when intensity is 0 (the
        // resolve then adds bloom*0, so stale mip contents are harmless).
        if self.tonemap_params.bloom_intensity > 0.0 {
            if let Some(bloom) = self.bloom.as_ref() {
                bloom.upload_params(
                    &self.queue,
                    self.tonemap_params.bloom_threshold,
                    self.tonemap_params.bloom_knee,
                    1.0,
                );
                bloom.render(encoder);
            }
        }
        // Eye adaptation: reduce the scene to average luminance and ease the exposure
        // scale (the resolve multiplies exposure by it). Always run on the HDR path —
        // when disabled it just eases to 1.0 — the reduction is a few cheap passes.
        if let Some(exposure) = self.exposure.as_ref() {
            exposure.upload_params(&self.queue, &self.exposure_params);
            exposure.render(encoder);
        }
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
        let msaa = self.sample_count > 1;
        // The scene colour target. MSAA: RENDER_ATTACHMENT only — it is resolved, never sampled.
        // 1x: also TEXTURE_BINDING, since the tonemap/bloom/exposure sample it directly.
        let usage = if msaa {
            wgpu::TextureUsages::RENDER_ATTACHMENT
        } else {
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgr_hdr_target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: self.sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // MSAA: a single-sample resolve target the scene colour is resolved into (run_tonemap
        // records the resolve); the post-processing chain samples it. 1x: none, and the chain
        // samples the scene target itself.
        let resolve = msaa.then(|| {
            let t = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("wgr_hdr_resolve"),
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
            let v = t.create_view(&wgpu::TextureViewDescriptor::default());
            (t, v)
        });
        // The single-sample view the post-processing chain reads (resolve target under MSAA,
        // else the scene target directly).
        let sample_view = resolve.as_ref().map(|(_, v)| v).unwrap_or(&view).clone();
        // Rebuild the bloom pyramid for the new size, then point the resolve at both
        // the HDR target and the bloom mip0. A 1x1 fallback keeps set_source valid if
        // the pyramid somehow has no mips.
        if let Some(bloom) = self.bloom.as_mut() {
            bloom.resize(&self.device, width, height, HDR_FORMAT, &sample_view);
        }
        if let Some(exposure) = self.exposure.as_mut() {
            exposure.resize(&self.device, width, height, &sample_view);
        }
        if let Some(tonemap) = self.tonemap.as_mut() {
            let bloom_view = self
                .bloom
                .as_ref()
                .and_then(|b| b.view())
                .unwrap_or(&sample_view);
            let scale_view = self
                .exposure
                .as_ref()
                .map(|e| e.scale_view())
                .unwrap_or(&sample_view);
            tonemap.set_source(&self.device, &sample_view, bloom_view, scale_view);
        }
        self.hdr = Some((texture, view));
        self.hdr_resolve = resolve;
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
        water_nodes: &[WgrWaterNode],
        water_batches: &[WgrWaterBatch],
    ) -> Result<(), String> {
        let screen = glam::Vec2::new(self.config.width as f32, self.config.height as f32);
        self.gfx2d
            .prepare(&self.device, &self.queue, screen, fog, verts);
        self.gfx2d
            .prepare_overlay(&self.device, &self.queue, overlay_verts, overlay_indices);
        self.gfx3d
            .ensure_depth(&self.device, self.config.width, self.config.height);
        // Rebuild the bindless object-texture array if any texture was created/destroyed
        // since last frame (no-op on churn-free frames), so this frame's draws index a
        // current array.
        self.textures.ensure_bindless(&self.device);
        // Compute skin bake (docs/compute-skin-bake-plan.md): plan + upload BEFORE both
        // prepare_shadows and prepare so those pack an identity world for every baked
        // draw/caster. Spans both draws and casters (one bake per skinned mesh+pose).
        self.gfx3d
            .prepare_skin_bake(&self.device, &self.queue, draws3d, shadow_casters, palette);
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
        let mut plan = self.gfx3d.plan_3d(cmds, draws3d);
        // Stage 2: turn the plan's instanceable buckets into CPU-built indirect draws over
        // the geometry pool (no-op when indirect is off). Tags each eligible op with its
        // args-buffer offset for the replay below.
        self.gfx3d
            .build_indirect(&self.device, &self.queue, draws3d, &mut plan.ops);
        self.gfx3d.prepare(
            &self.device,
            &self.queue,
            &self.textures,
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
            self.sky.froxel_view(),
        );
        self.terrain
            .prepare(&self.device, &self.queue, terrain_nodes);
        self.water
            .prepare(&self.device, &self.queue, water_nodes);
        // GPU-driven rendering (Stage 3): upload the retained scene + this frame's cull
        // params from the main scene camera (the terrain camera, else the first 3D draw's).
        // No-op when disabled; inert until C++ registers a scene.
        let gpu_main_cam = terrain_batches
            .first()
            .map(|b| b.camera as usize)
            .or_else(|| draws3d.first().map(|d| d.camera as usize))
            .unwrap_or(0);
        if let Some(cam) = cameras.get(gpu_main_cam) {
            self.gfx3d.prepare_cull(&self.device, &self.queue, cam, shadow);
        }
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
        // MSAA only: the single-sample depth the post-tonemap UI phase attaches instead of the
        // multisampled scene depth (its 2D draws composite to the 1x swapchain). None at 1x, where
        // `depth` is already single-sample and serves both phases.
        let ui_depth = self.gfx3d.ui_depth_view().cloned();
        // The prepass' view-space normal G-buffer target (None when the prepass is
        // disabled). Cloned (Arc) so no borrow of self is held across the segment loop.
        let normal = if self.prepass_enabled {
            Some(
                self.gfx3d
                    .normal_view()
                    .ok_or("normal target missing")?
                    .clone(),
            )
        } else {
            None
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wgr_frame"),
            });

        // Compute skin bake runs first of all (docs/compute-skin-bake-plan.md): it writes
        // the shared skinned vertex buffer that the shadow cascades, the depth prepass, and
        // the forward pass all read, so it must precede them; wgpu inserts the storage->
        // vertex barrier. No-op when the bake is off or there is no skinned geometry.
        self.gfx3d.skin_bake(&mut encoder);

        // GPU cull compute (Stage 3): culls + LOD-selects the retained scene into the
        // indirect args the colour pass consumes. Recorded before the render passes so
        // wgpu barriers its storage writes -> the indirect reads. No-op when disabled.
        self.gfx3d.cull_dispatch(&mut encoder);
        // One extra cull dispatch per shadow cascade (§6 multi-view): each produces that
        // cascade's depth-pass indirect args, consumed by the GPU-driven shadow draw inside
        // render_shadow_passes. No-op when GPU-driven / shadows are off.
        self.gfx3d.cull_dispatch_shadows(&mut encoder);

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

        // Procedural sky (docs/procedural-sky-plan.md): a fullscreen atmospheric pass
        // into the scene target BEFORE any geometry, so terrain/objects overdraw it.
        // Depth is untouched (the pass has none). Skipped when disabled or camera-less;
        // when it runs it fills every pixel, so the first segment loads over it instead
        // of clearing. The legacy skydome meshes are suppressed on the C++ side.
        // Reconstruct the inverse view-projection (for world ray directions) from the
        // camera the VISIBLE GEOMETRY draws with — the first terrain batch, else the
        // first 3D draw — not cameras[0]. When several cameras are pushed in a frame
        // (e.g. flying: main view + cockpit/optics), cameras[0] can be a stale or
        // secondary view, which makes the sky (and thus the sun disc + horizon haze)
        // stutter against the terrain as the player moves. None = disabled / no camera.
        let sky_ivp = if self.sky_params.control[0] != 0.0 {
            let main_cam = terrain_batches
                .first()
                .map(|b| b.camera as usize)
                .or_else(|| draws3d.first().map(|d| d.camera as usize))
                .unwrap_or(0);
            if self.sky_debug {
                let cur = (cameras.len(), main_cam);
                if cur != self.sky_dbg_last {
                    self.sky_dbg_last = cur;
                    self.log.log(
                        log_level::INFO,
                        &format!("wgr_sky: cameras={} main_cam={}", cur.0, cur.1),
                    );
                }
            }
            cameras.get(main_cam).map(|cam| {
                // Reconstruct inv(proj*view) = inv(view) * inv(proj), inverting the two
                // matrices SEPARATELY and in f64. Our projection is reversed-Z with an
                // infinite far plane (ill-conditioned z-row); inverting the *combined*
                // f32 matrix smears that poor conditioning into the x/y ray components,
                // which shows up as horizon jitter when the orientation changes fast
                // (pitching while flying). inv(view) alone has no such pathology, and the
                // split keeps the projection's conditioning out of x/y. The view already
                // has its translation zeroed (see EngineWgpu::PushSceneCamera), so the
                // result is translation-invariant.
                let view = glam::DMat4::from_cols_array(&cam.view.map(f64::from));
                let proj = glam::DMat4::from_cols_array(&cam.proj.map(f64::from));
                let inv_vp = view.inverse() * proj.inverse();
                let m = inv_vp.as_mat4().to_cols_array();
                let ivp = [
                    [m[0], m[1], m[2], m[3]],
                    [m[4], m[5], m[6], m[7]],
                    [m[8], m[9], m[10], m[11]],
                    [m[12], m[13], m[14], m[15]],
                ];
                // Absolute world camera position (the froxel occlusion needs it to place a
                // marched camera-relative offset onto the world-space terrain shadow mask),
                // plus this camera's cascade matrices for the froxel's near-field CSM occlusion.
                (ivp, cam.cam_pos, cam.shadow)
            })
        } else {
            None
        };
        let sky_drawn = sky_ivp.is_some();
        if let Some((ivp, cam_pos, cam_shadow)) = sky_ivp {
            self.sky.upload(
                &self.queue,
                &self.sky_params,
                ivp,
                cam_pos,
                &shadow_mapping,
                &cam_shadow,
            );
            // Rebuild the transmittance + multiscatter LUTs first if the atmosphere
            // changed (no-op most frames), then draw the fullscreen sky.
            self.sky.render_luts(&mut encoder);
            // Fill the aerial-perspective froxel volume from the fresh uniform + LUTs, now
            // occluded by the terrain sun-shadow mask (far) + cascade shadow map (near) so
            // the sun can't bleed through hills OR objects, and casters carve god-ray shafts.
            self.sky.render_froxel(
                &self.device,
                &mut encoder,
                &shadow_mask_view,
                self.gfx3d.shadow_sample_view(),
            );
            encoder.push_debug_group("wgr_sky");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_sky"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &scene_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_rgb[0] as f64,
                            g: clear_rgb[1] as f64,
                            b: clear_rgb[2] as f64,
                            a: clear[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.sky.render(&mut pass);
            drop(pass);
            encoder.pop_debug_group();
        }

        // Fog is now applied per-fragment in the forward shaders by sampling the aerial
        // froxel volume (filled above), so there is no deferred fog pass between the 3D
        // and 2D sub-passes — the 2D overlays simply never sample it.

        // `target` is where the current phase's segments render (HDR then swapchain);
        // `display_2d` picks the swapchain-format 2D pipelines in the UI phase.
        let mut target = scene_view.clone();
        let mut display_2d = false;
        // If the sky filled the target, segments load over it; else the first clears.
        let mut clear_color_next = !sky_drawn;
        let mut resolved = false;
        let mut start = 0usize;
        let mut seg_idx = 0usize;

        // Aerial perspective is a DEFERRED pass over the scene depth, so it must run after
        // all foggable 3D world geometry but before the 2D overlays (HUD / sights / scope),
        // which have no world depth and must never be fogged. 2D and 3D draws are
        // interleaved in the stream and which 2D draws exist changes frame to frame
        // (markers, icons), so a "split at the first 2D op" is unstable — it would strand
        // every later 3D object in the un-fogged tail, and flicker as those 2D draws come
        // and go. Instead PARTITION each segment: replay all non-2D ops, fog, then replay
        // all 2D ops. `want_2d` selects which side to draw (order preserved within each);
        // `display_2d` is threaded as a param (not captured) so the outer mutable changes.
        // `depth_write_off` = this is the prepassed segment's colour pass, so its opaque
        // set (objects + terrain) draws over the already-complete depth GreaterEqual/
        // write-off. False for post-ClearDepth segments (no prepass) and the 2D sub-pass.
        let render_ops = |pass: &mut wgpu::RenderPass<'_>,
                          sub: &[Plan3dOp],
                          display_2d: bool,
                          want_2d: bool,
                          depth_write_off: bool| {
            let mut st3d = crate::gfx3d::Pass3dState::default();
            for op in sub {
                if matches!(op, Plan3dOp::Draw2D(_)) != want_2d {
                    continue;
                }
                match op {
                    Plan3dOp::Draw2D(arg) => {
                        st3d = crate::gfx3d::Pass3dState::default();
                        if let Some(b) = batches.get(*arg as usize) {
                            self.gfx2d.draw_one(pass, &self.textures, b, display_2d);
                        }
                    }
                    Plan3dOp::Draw3D {
                        draw,
                        base,
                        count,
                        kind,
                    } => {
                        if let Some(d) = draws3d.get(*draw as usize) {
                            let mode = crate::gfx3d::Pass3dMode::Color { depth_write_off };
                            if let crate::gfx3d::DrawKind::Indirect(off) = kind {
                                self.gfx3d
                                    .draw_indirect(pass, &self.textures, d, *off, &mut st3d, mode);
                            } else {
                                self.gfx3d.draw_one(
                                    pass,
                                    &self.textures,
                                    d,
                                    *base,
                                    *count,
                                    &mut st3d,
                                    mode,
                                );
                            }
                        }
                    }
                    Plan3dOp::Terrain(arg) => {
                        st3d = crate::gfx3d::Pass3dState::default();
                        if let (Some(b), Some(cam)) =
                            (terrain_batches.get(*arg as usize), self.gfx3d.camera_bind())
                        {
                            let off = (b.camera as u64 * self.gfx3d.camera_stride()) as u32;
                            let kind = if depth_write_off {
                                crate::terrain::TerrainPass::ColorNoWrite
                            } else {
                                crate::terrain::TerrainPass::Color
                            };
                            self.terrain
                                .draw(pass, cam, off, b.first_node, b.node_count, kind);
                        }
                    }
                    // Water is drawn in a dedicated pass after this sub-pass (it samples the
                    // opaque depth it also depth-tests against, which needs a read-only depth
                    // attachment the shared colour sub-pass can't give). Skipped here.
                    Plan3dOp::Water(_) => {}
                    Plan3dOp::ClearDepth | Plan3dOp::Resolve => {}
                }
            }
        };

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
            let seg_ops = &ops[start..end];
            let has_2d = seg_ops.iter().any(|o| matches!(o, Plan3dOp::Draw2D(_)));

            // Depth+normal prepass over the FIRST (world) depth segment only
            // (docs/depth-prepass-plan.md, decision 5): start == 0 marks it. The prepass
            // replays the segment's opaque set (objects self-filter; terrain always) into
            // the normal G-buffer + depth (cleared 0.0 reversed-Z, stencil cleared 0). The
            // colour sub-pass below then LOADS this depth and draws the opaque set early-Z
            // with depth-write off. Later segments (near/weapon) keep the single-pass path.
            let do_prepass = start == 0;
            if let (true, Some(normal_view)) = (do_prepass, normal.as_ref()) {
                encoder.push_debug_group("wgr_depth_prepass");
                let mut pp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("wgr_depth_prepass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: normal_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
                        // Clear stencil to 0 here so the colour pass can LOAD it (the
                        // shadow-darken pass wants stencil == 0 to start).
                        stencil_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0),
                            store: wgpu::StoreOp::Store,
                        }),
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                let mut st3d = crate::gfx3d::Pass3dState::default();
                for op in seg_ops {
                    match op {
                        Plan3dOp::Draw3D {
                            draw,
                            base,
                            count,
                            kind,
                        } => {
                            if let Some(d) = draws3d.get(*draw as usize) {
                                let mode = crate::gfx3d::Pass3dMode::Prepass;
                                if let crate::gfx3d::DrawKind::Indirect(off) = kind {
                                    self.gfx3d.draw_indirect(
                                        &mut pp,
                                        &self.textures,
                                        d,
                                        *off,
                                        &mut st3d,
                                        mode,
                                    );
                                } else {
                                    self.gfx3d.draw_one(
                                        &mut pp,
                                        &self.textures,
                                        d,
                                        *base,
                                        *count,
                                        &mut st3d,
                                        mode,
                                    );
                                }
                            }
                        }
                        Plan3dOp::Terrain(arg) => {
                            st3d = crate::gfx3d::Pass3dState::default();
                            if let (Some(b), Some(cam)) =
                                (terrain_batches.get(*arg as usize), self.gfx3d.camera_bind())
                            {
                                let off = (b.camera as u64 * self.gfx3d.camera_stride()) as u32;
                                self.terrain.draw(
                                    &mut pp,
                                    cam,
                                    off,
                                    b.first_node,
                                    b.node_count,
                                    crate::terrain::TerrainPass::Prepass,
                                );
                            }
                        }
                        _ => {}
                    }
                }
                // GPU-driven opaque set into the SAME depth+normal prepass (reuses this
                // frame's cull out_args — the cull dispatch already ran before the passes).
                // Writes depth + view-space normals so the set gets early-Z + SSAO normals.
                if !self.suppress_world_objects {
                    let cam_off = (gpu_main_cam as u64 * self.gfx3d.camera_stride()) as u32;
                    self.gfx3d
                        .draw_gpu_driven_prepass(&mut pp, &self.textures, cam_off);
                }
                drop(pp);
                encoder.pop_debug_group();
            }
            // When the prepass ran, the depth (+ stencil 0) it wrote is complete, so the
            // colour sub-pass LOADS it; otherwise it clears as before.
            let prepassed = do_prepass && normal.is_some();

            // GPU Hi-Z occlusion (docs/gpu-culling-and-depth-plan.md §5): now that the prepass
            // depth is complete (terrain + CPU objects + the GPU-driven set), reduce it to a Hi-Z
            // pyramid and run the color-pass occlusion cull. Both no-op unless occlusion is active.
            // Recorded between the prepass and colour passes so wgpu barriers depth-write -> Hi-Z
            // read -> color-cull sample -> the colour draw's indirect read. The colour draw
            // (draw_gpu_driven, below) then consumes the occlusion-culled args.
            if prepassed {
                self.gfx3d.build_hiz(&self.device, &mut encoder);
                self.gfx3d.cull_dispatch_color(&mut encoder);
            }

            // Depth attachment for this segment's sub-passes. Post-tonemap (resolved) the target is
            // the 1x swapchain, so under MSAA attach the single-sample UI depth instead of the
            // multisampled scene depth (matching sample counts). Pre-resolve, and always at 1x,
            // it's the scene depth.
            let seg_depth: &wgpu::TextureView = if resolved {
                ui_depth.as_ref().unwrap_or(&depth)
            } else {
                &depth
            };

            // 3D sub-pass: all non-2D draws. Depth/stencil are cleared here only when the
            // prepass didn't already fill them (stencil to 0 so shadow draws EQUAL 0 /
            // INCR darken each pixel once); colour per the load.
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
                        view: seg_depth,
                        depth_ops: Some(wgpu::Operations {
                            load: if prepassed {
                                wgpu::LoadOp::Load
                            } else {
                                // Reversed-Z: far plane is 0
                                wgpu::LoadOp::Clear(0.0)
                            },
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: Some(wgpu::Operations {
                            load: if prepassed {
                                wgpu::LoadOp::Load
                            } else {
                                wgpu::LoadOp::Clear(0)
                            },
                            store: wgpu::StoreOp::Store,
                        }),
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                // GPU-driven opaque world objects (Stage 3), in the world segment only
                // (start == 0). Drawn BEFORE the CPU ops: it is depth-tested opaque so its order
                // vs the CPU OPAQUE set is irrelevant, but the CPU set also contains alpha-BLENDED
                // draws (fences, glass) that read the framebuffer colour — those must blend
                // against the GPU-driven objects behind them, not the background, so the
                // GPU-driven colour has to be present first (else the sky shows through).
                if start == 0 && !self.suppress_world_objects {
                    let cam_off = (gpu_main_cam as u64 * self.gfx3d.camera_stride()) as u32;
                    self.gfx3d.draw_gpu_driven(&mut pass, &self.textures, cam_off);
                }
                render_ops(&mut pass, seg_ops, display_2d, false, prepassed);
                // Debug cull-sphere wireframes (ImGui Culling tab) LAST in the sub-pass: their
                // depth test is Always, but anything drawn after them (terrain in render_ops)
                // would still overwrite their colour — so they must follow every world draw to
                // actually show on top.
                if start == 0 && !self.suppress_world_objects && self.cull_debug_draw {
                    let cam_off = (gpu_main_cam as u64 * self.gfx3d.camera_stride()) as u32;
                    self.gfx3d.draw_cull_spheres(&mut pass, cam_off);
                }
                drop(pass);
            }

            // Dedicated water pass. Water is transparent and reconstructs the seabed by SAMPLING
            // the opaque prepass depth — which it also depth-tests against — so its depth
            // attachment must be READ-ONLY (depth_ops/stencil_ops = None). The shared colour
            // sub-pass can't be read-only (the GPU-driven opaque pipeline writes depth), so water
            // draws here instead, after the opaque set, loading colour + read-only depth. Water
            // still depth-tests vs the coast (GreaterEqual) and writes no depth. Pre-resolve only
            // (the world segment); the resolved MSAA depth that water samples is filled by the
            // depth-resolve run before this segment's colour pass.
            let has_water = seg_ops.iter().any(|o| matches!(o, Plan3dOp::Water(_)));
            if has_water && !resolved {
                let dgen = self.gfx3d.depth_gen();
                if let Some(dv) = self.gfx3d.depth_sample_view() {
                    self.water.set_depth_view(&self.device, dv, dgen);
                }
                if let Some(cam) = self.gfx3d.camera_bind() {
                    encoder.push_debug_group("wgr_water");
                    let mut wpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("wgr_water_pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: seg_depth,
                            depth_ops: None,
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    for op in seg_ops {
                        if let Plan3dOp::Water(arg) = op {
                            if let Some(b) = water_batches.get(*arg as usize) {
                                let off = (b.camera as u64 * self.gfx3d.camera_stride()) as u32;
                                self.water
                                    .draw(&mut wpass, cam, off, b.first_node, b.node_count);
                            }
                        }
                    }
                    drop(wpass);
                    encoder.pop_debug_group();
                }
            }

            // 2D sub-pass: the overlays, over the fogged colour, loading the 3D depth +
            // stencil so any depth-tested 2D still occludes and stencil state carries over.
            if has_2d {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(&seg_label),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: seg_depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                render_ops(&mut pass, seg_ops, display_2d, true, false);
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
        self.gfx3d
            .mesh_create(&self.device, &self.queue, verts, indices)
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

    // --- GPU-driven retained scene (docs/gpu-culling-and-depth-plan.md Stage 3b) ---

    fn model_register(
        &mut self,
        bounding_sphere: f32,
        lods: &[WgrModelLod],
        sections: &[WgrModelSection],
        materials: &[WgrModelMaterial],
    ) -> u32 {
        self.gfx3d
            .register_model(bounding_sphere, lods, sections, materials, &self.textures)
    }

    fn instance_add(&mut self, inst: &WgrInstance) -> u32 {
        self.gfx3d.instance_add(inst)
    }

    fn instance_update(&mut self, slot: u32, inst: &WgrInstance) {
        self.gfx3d.instance_update(slot, inst);
    }

    fn instance_remove(&mut self, slot: u32) {
        self.gfx3d.instance_remove(slot);
    }

    fn set_dynamic(&mut self, instances: &[WgrInstance]) {
        self.gfx3d.set_dynamic(instances);
    }

    // Push the engine's per-frame cull + LOD inputs (the real Scene::LevelFromDistance2 values).
    fn set_cull_inputs(&mut self, objects_z: f32, lod_scale: f32, lod_inv_width: f32, pixel_limit: f32) {
        self.gfx3d
            .set_cull_inputs(objects_z, lod_scale, lod_inv_width, pixel_limit);
    }

    // Per-frame: suppress the retained GPU-driven world set (objects + prepass) so the
    // editor/loading/shutdown frames don't leak clutter behind the 2D UI. Resources stay
    // resident; only this frame's draw submission is skipped. C++ sets it every frame.
    fn set_suppress_world_objects(&mut self, suppress: bool) {
        self.suppress_world_objects = suppress;
    }

    // ImGui Culling tab (wgr_set_cull_debug): draw the cull-sphere wireframes, skip the GPU
    // frustum test, and toggle GPU Hi-Z occlusion. First two are diagnostics for the GPU-driven
    // "objects vanish / float" investigation; occlusion is the §5 Hi-Z cull.
    fn set_cull_debug(&mut self, draw_spheres: bool, no_frustum: bool, occlusion: bool) {
        self.cull_debug_draw = draw_spheres;
        self.gfx3d.set_cull_no_frustum(no_frustum);
        self.gfx3d.set_occlusion_enabled(occlusion);
    }

    fn terrain_set_heightmap(&mut self, heights: &[f32], params: WgrTerrainParams) {
        self.terrain
            .set_heightmap(&self.device, &self.queue, heights, params);
    }

    fn water_set_params(&mut self, params: WgrWaterParams) {
        self.water.set_params(&self.queue, params);
    }

    fn terrain_set_params(&mut self, params: WgrTerrainParams) {
        self.terrain.set_params(&self.queue, params);
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
