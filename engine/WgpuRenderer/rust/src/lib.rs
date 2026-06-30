mod ffi;
mod gfx2d;
mod gfx3d;
mod handles;
mod log;
mod textures;

use crate::ffi::{WgrDraw2DBatch, WgrDraw3D, WgrMeshVertex, WgrVertex2D};
use crate::gfx2d::Gfx2d;
use crate::gfx3d::Gfx3d;
use crate::log::{LogSink, log_level};
use crate::textures::{SharedTextures, TextureFormat, TextureData};

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
            &format!("wgpu adapter: {} ({:?}, {:?})", info.name, info.backend, info.device_type),
        );

        let bc_features = adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
        let bc_supported = !bc_features.is_empty();
        if !bc_supported {
            log.log(log_level::WARN, "wgpu adapter lacks TEXTURE_COMPRESSION_BC; DXT textures will fail to upload");
        }

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: bc_features,
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
        let gfx2d = Gfx2d::new(&device, &textures, config.format);
        let gfx3d = Gfx3d::new(&device, &textures, config.format);

        Ok(Self { log, surface, device, queue, config, textures, gfx2d, gfx3d })
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

    fn clear_and_present(&mut self, r: f32, g: f32, b: f32, a: f32) -> Result<(), String> {
        self.render_2d([r, g, b, a], &[], &[])
    }

    fn render_2d(&mut self, clear: [f32; 4], verts: &[WgrVertex2D], batches: &[WgrDraw2DBatch]) -> Result<(), String> {
        self.render_frame(clear, &ffi::IDENTITY, &ffi::IDENTITY, &[], verts, batches)
    }

    fn render_frame(&mut self, clear: [f32; 4], proj: &[f32; 16], view: &[f32; 16], draws3d: &[WgrDraw3D],
                    verts: &[WgrVertex2D], batches: &[WgrDraw2DBatch]) -> Result<(), String> {
        let screen = glam::Vec2::new(self.config.width as f32, self.config.height as f32);
        self.gfx2d.prepare(&self.device, &self.queue, screen, verts);
        self.gfx3d.ensure_depth(&self.device, self.config.width, self.config.height);
        self.gfx3d.prepare(&self.device, &self.queue, proj, view, draws3d);

        let Some(frame) = self.acquire()? else { return Ok(()) };

        let color = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = self.gfx3d.depth_view().ok_or("depth target missing")?;
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("wgr_frame") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_3d_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0] as f64,
                            g: clear[1] as f64,
                            b: clear[2] as f64,
                            a: clear[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.gfx3d.draw(&mut pass, &self.textures, draws3d);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_2d_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.gfx2d.draw(&mut pass, &self.textures, batches);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    fn texture_create(&mut self, width: u32, height: u32, format: TextureFormat, data: &[u8]) -> u64 {
        self.textures.create(&self.device, &self.queue, &TextureData { width, height, format, bytes: data })
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

    fn mesh_destroy(&mut self, handle: u64) {
        self.gfx3d.mesh_destroy(handle);
    }
}
