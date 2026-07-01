use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
    Win32WindowHandle, WindowsDisplayHandle, XlibDisplayHandle, XlibWindowHandle,
};

use crate::ffi::{WgrPlatform, WgrSurfaceDesc};

#[cfg(windows)]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut core::ffi::c_void;
}

pub fn build_handles(desc: &WgrSurfaceDesc) -> Result<(RawDisplayHandle, RawWindowHandle), String> {
    match desc.platform {
        WgrPlatform::Win32 => {
            let hwnd = std::num::NonZeroIsize::new(desc.window as isize)
                .ok_or_else(|| "win32: null HWND".to_string())?;
            let mut handle = Win32WindowHandle::new(hwnd);
            #[cfg(windows)]
            {
                let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) } as isize;
                handle.hinstance = std::num::NonZeroIsize::new(hinstance);
            }
            Ok((
                RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
                RawWindowHandle::Win32(handle),
            ))
        }
        WgrPlatform::Xlib => {
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
        WgrPlatform::Wayland => {
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
