mod ffi;

use std::ffi::{CString, c_void};
use std::os::raw::c_char;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, Win32WindowHandle,
    WindowsDisplayHandle, XlibDisplayHandle, XlibWindowHandle,
};

// Mirrors `WgrLogLevel` in the C header
pub mod log_level {
    pub const TRACE: i32 = 0;
    pub const DEBUG: i32 = 1;
    pub const INFO: i32 = 2;
    pub const WARN: i32 = 3;
    pub const ERROR: i32 = 4;
}

#[derive(Clone, Copy)]
struct LogSink {
    cb: Option<extern "C" fn(level: i32, msg: *const c_char, user: *mut c_void)>,
    user: *mut c_void,
}

impl LogSink {
    fn log(&self, level: i32, msg: &str) {
        if let Some(cb) = self.cb {
            if let Ok(c) = CString::new(msg) {
                cb(level, c.as_ptr(), self.user);
            }
        }
    }
}

pub struct Renderer {
    log: LogSink,
    // The surface is `'static`: it's built from raw handles and C++ keeps the window alive until after `wgr_destroy`
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

impl Renderer {
    fn new(desc: &ffi::WgrSurfaceDesc, log: LogSink) -> Result<Self, String> {
        let (raw_display_handle, raw_window_handle) = build_handles(desc)?;

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

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| format!("request_device failed: {e}"))?;

        let config = surface
            .get_default_config(&adapter, desc.width.max(1), desc.height.max(1))
            .ok_or_else(|| "surface is not supported by the chosen adapter".to_string())?;
        surface.configure(&device, &config);

        Ok(Self { log, surface, device, queue, config })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn clear_and_present(&mut self, r: f32, g: f32, b: f32, a: f32) -> Result<(), String> {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            Cst::Success(t) | Cst::Suboptimal(t) => t,
            // The surface needs reconfiguring; do it and skip this frame.
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Cst::Timeout | Cst::Occluded => return Ok(()),
            Cst::Validation => return Err("get_current_texture: validation error".to_string()),
        };

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("wgr_clear") });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("wgr_clear_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r as f64,
                            g: g as f64,
                            b: b as f64,
                            a: a as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }
}

fn build_handles(desc: &ffi::WgrSurfaceDesc) -> Result<(RawDisplayHandle, RawWindowHandle), String> {
    match desc.platform {
        ffi::WgrPlatform::Win32 => {
            let hwnd = std::num::NonZeroIsize::new(desc.window as isize)
                .ok_or_else(|| "win32: null HWND".to_string())?;
            Ok((
                RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
                RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)),
            ))
        }
        ffi::WgrPlatform::Xlib => {
            let window = desc.window as usize as core::ffi::c_ulong;
            if window == 0 {
                return Err("xlib: null window".to_string());
            }
            let display = std::ptr::NonNull::new(desc.display);
            Ok((
                RawDisplayHandle::Xlib(XlibDisplayHandle::new(display, 0)),
                RawWindowHandle::Xlib(XlibWindowHandle::new(window)),
            ))
        }
        ffi::WgrPlatform::Wayland => {
            let surface = std::ptr::NonNull::new(desc.window)
                .ok_or_else(|| "wayland: null wl_surface".to_string())?;
            let display = std::ptr::NonNull::new(desc.display)
                .ok_or_else(|| "wayland: null wl_display".to_string())?;
            Ok((
                RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display)),
                RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
            ))
        }
    }
}
