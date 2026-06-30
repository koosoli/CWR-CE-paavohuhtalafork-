use std::ffi::c_void;
use std::os::raw::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};

use glam::{Vec2, Vec3};

use crate::Renderer;
use crate::log::{LogSink, log_level};
use crate::textures::TextureFormat;

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

// Mirror of the C++ `Sampler2DFlags` / GL33's `_samplerObjects` index. The bits
// double as the index into the renderer's 8 samplers.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct WgrSampler2D(pub u32);

impl WgrSampler2D {
    pub const CLAMP_U: u32 = 1;
    pub const CLAMP_V: u32 = 2;
    pub const POINT: u32 = 4;

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrDraw2DBatch {
    pub texture_id: u64,
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub blend: WgrBlend,
    pub sampler: WgrSampler2D,
}

// Object-space mesh vertex; matches the engine's SVertex (pos, normal, uv).
// glam types are #[repr(C)] and ABI-identical to [f32; 3]/[f32; 2].
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrMeshVertex {
    pub pos: Vec3,
    pub norm: Vec3,
    pub uv: Vec2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrDraw3D {
    pub mesh: u64,
    pub index_begin: u32,
    pub index_count: u32,
    pub texture_id: u64,
    // Column-major. Must stay [f32; 16] (align 4) to match the C side's
    // float[16]; glam::Mat4 has 16-byte align and would change the struct layout.
    pub world: [f32; 16],
    pub blend: WgrBlend,
    pub sampler: WgrSampler2D,
}

// Layouts must match wgpu_renderer.h exactly (the C side static_asserts the same).
const _: () = assert!(std::mem::size_of::<WgrMeshVertex>() == 32);
const _: () = assert!(std::mem::size_of::<WgrDraw3D>() == 96);

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
        let Some(fmt) = TextureFormat::from_i32(format) else {
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

/// # Safety
/// `renderer` must be live; `verts`/`indices` must each point to at least the
/// given number of elements, or be null (in which case 0 is returned).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_mesh_create(
    renderer: *mut WgrRenderer,
    verts: *const WgrMeshVertex,
    vert_count: u32,
    indices: *const u16,
    index_count: u32,
) -> u64 {
    if renderer.is_null() || verts.is_null() || indices.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let verts = unsafe { std::slice::from_raw_parts(verts, vert_count as usize) };
        let indices = unsafe { std::slice::from_raw_parts(indices, index_count as usize) };
        renderer.mesh_create(verts, indices)
    }))
    .unwrap_or(0)
}

/// # Safety
/// `renderer` must be a live pointer from `wgr_create`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_mesh_destroy(renderer: *mut WgrRenderer, id: u64) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        unsafe { &mut *renderer }.mesh_destroy(id);
    }));
}

/// # Safety
/// `renderer` must be live. `proj`/`view` must each point to 16 floats or be
/// null (treated as identity). Each draw/vertex/batch array may be null only
/// when its count is 0; otherwise it must hold the given number of elements.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn wgr_render_frame(
    renderer: *mut WgrRenderer,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
    proj: *const f32,
    view: *const f32,
    draws3d: *const WgrDraw3D,
    draw_count: u32,
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
        let read_mat = |p: *const f32| -> [f32; 16] {
            if p.is_null() {
                IDENTITY
            } else {
                let mut m = [0.0f32; 16];
                m.copy_from_slice(unsafe { std::slice::from_raw_parts(p, 16) });
                m
            }
        };
        let proj = read_mat(proj);
        let view = read_mat(view);
        let draws3d: &[WgrDraw3D] = if draws3d.is_null() || draw_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(draws3d, draw_count as usize) }
        };
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
        match renderer.render_frame([r, g, b, a], &proj, &view, draws3d, verts, batches) {
            Ok(()) => 0,
            Err(e) => {
                renderer.log.log(log_level::ERROR, &format!("render_frame: {e}"));
                -2
            }
        }
    }))
    .unwrap_or(-3)
}

#[rustfmt::skip]
pub(crate) const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0,
    0.0, 1.0, 0.0, 0.0,
    0.0, 0.0, 1.0, 0.0,
    0.0, 0.0, 0.0, 1.0,
];
