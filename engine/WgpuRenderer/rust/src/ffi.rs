use std::ffi::c_void;
use std::os::raw::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{LogSink, Renderer, log_level};

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

/// Opaque on the C side; a heap-allocated [`Renderer`].
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
            None => LogSink { cb: None, user: std::ptr::null_mut() },
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
