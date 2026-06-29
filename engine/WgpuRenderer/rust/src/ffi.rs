use std::ffi::c_void;
use std::os::raw::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};

use glam::Vec2;

use crate::Renderer;
use crate::gfx2d::TexFormat;
use crate::log::{LogSink, log_level};

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum WgrPlatform {
    Win32 = 0,
    Xlib = 1,
    Wayland = 2,
}

#[repr(C)]
pub struct WgrSurfaceDesc {
    pub platform: WgrPlatform,
    pub window: *mut c_void,
    pub display: *mut c_void,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
pub struct WgrLogCallbacks {
    pub log: Option<extern "C" fn(level: i32, msg: *const c_char, user: *mut c_void)>,
    pub user: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrVertex2D {
    pub pos: Vec2,
    pub uv: Vec2,
    pub color: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum WgrBlend {
    Opaque = 0,
    Alpha = 1,
    Additive = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrDraw2DBatch {
    pub texture_id: u64,
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub blend: WgrBlend,
}

pub type WgrRenderer = Renderer;

#[unsafe(no_mangle)]
pub extern "C" fn wgr_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// # Safety
/// `desc` must point to a valid `WgrSurfaceDesc` and `log` to a valid
/// `WgrLogCallbacks` or be null. The window in `desc` must outlive the renderer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_create(
    desc: *const WgrSurfaceDesc,
    log: *const WgrLogCallbacks,
) -> *mut WgrRenderer {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(desc) = (unsafe { desc.as_ref() }) else {
            return std::ptr::null_mut();
        };
        let sink = match unsafe { log.as_ref() } {
            Some(l) => LogSink { cb: l.log, user: l.user },
            None => LogSink::none(),
        };
        match Renderer::new(desc, sink) {
            Ok(renderer) => {
                sink.log(log_level::INFO, "wgpu renderer created");
                Box::into_raw(Box::new(renderer))
            }
            Err(e) => {
                sink.log(log_level::ERROR, &format!("wgpu renderer creation failed: {e}"));
                std::ptr::null_mut()
            }
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// # Safety
/// `renderer` must be a live pointer from `wgr_create` (not yet destroyed), or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_destroy(renderer: *mut WgrRenderer) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(unsafe { Box::from_raw(renderer) });
    }));
}

/// # Safety
/// `renderer` must be a live pointer from `wgr_create`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_resize(renderer: *mut WgrRenderer, width: u32, height: u32) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        unsafe { &mut *renderer }.resize(width, height);
    }));
}

/// # Safety
/// `renderer` must be a live pointer from `wgr_create`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_clear_and_present(
    renderer: *mut WgrRenderer,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) -> i32 {
    if renderer.is_null() {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        match renderer.clear_and_present(r, g, b, a) {
            Ok(()) => 0,
            Err(e) => {
                renderer.log.log(log_level::ERROR, &format!("clear_and_present: {e}"));
                -2
            }
        }
    }))
    .unwrap_or(-3)
}

/// # Safety
/// `renderer` must be live; `data` must point to at least `byte_len` bytes, or be
/// null (in which case 0 is returned).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_texture_create(
    renderer: *mut WgrRenderer,
    width: u32,
    height: u32,
    format: i32,
    data: *const u8,
    byte_len: u32,
) -> u64 {
    if renderer.is_null() || data.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let Some(fmt) = TexFormat::from_i32(format) else {
            return 0;
        };
        let renderer = unsafe { &mut *renderer };
        let slice = unsafe { std::slice::from_raw_parts(data, byte_len as usize) };
        renderer.texture_create(width, height, fmt, slice)
    }))
    .unwrap_or(0)
}

/// # Safety
/// `renderer` must be live; `data` must point to at least `byte_len` bytes, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_texture_update(
    renderer: *mut WgrRenderer,
    id: u64,
    data: *const u8,
    byte_len: u32,
) {
    if renderer.is_null() || data.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let slice = unsafe { std::slice::from_raw_parts(data, byte_len as usize) };
        renderer.texture_update(id, slice);
    }));
}

/// # Safety
/// `renderer` must be a live pointer from `wgr_create`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_texture_destroy(renderer: *mut WgrRenderer, id: u64) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        unsafe { &mut *renderer }.texture_destroy(id);
    }));
}

/// # Safety
/// `renderer` must be live. `verts`/`batches` may be null only when their count is
/// 0; otherwise each must point to at least the given number of elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_render_2d(
    renderer: *mut WgrRenderer,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    verts: *const WgrVertex2D,
    vert_count: u32,
    batches: *const WgrDraw2DBatch,
    batch_count: u32,
) -> i32 {
    if renderer.is_null() {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let verts: &[WgrVertex2D] = if verts.is_null() || vert_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(verts, vert_count as usize) }
        };
        let batches: &[WgrDraw2DBatch] = if batches.is_null() || batch_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(batches, batch_count as usize) }
        };
        match renderer.render_2d([r, g, b, a], verts, batches) {
            Ok(()) => 0,
            Err(e) => {
                renderer.log.log(log_level::ERROR, &format!("render_2d: {e}"));
                -2
            }
        }
    }))
    .unwrap_or(-3)
}
