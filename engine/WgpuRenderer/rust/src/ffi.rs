use std::ffi::c_void;
use std::os::raw::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::Renderer;
use crate::log::{LogSink, log_level};
use crate::textures::TextureFormat;

pub type WgrVec2 = glam::Vec2;
pub type WgrVec3 = glam::Vec3;
pub type WgrVec4 = [f32; 4];
pub type WgrMat4 = [f32; 16];

#[repr(C)]
pub struct WgrSlice<T> {
    pub data: *const T,
    pub len: u32,
}

impl<T> WgrSlice<T> {
    /// # Safety
    /// `data` must be null (only when `len` is 0) or point to at least `len`
    /// elements of `T` that outlive the returned slice.
    unsafe fn as_slice<'a>(&self) -> &'a [T] {
        if self.data.is_null() || self.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data, self.len as usize) }
        }
    }
}

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
    // pos.x/y = window pixels, pos.z = depth.
    pub pos: WgrVec3,
    pub rhw: f32,
    pub fog: f32,
    pub uv: WgrVec2,
    pub color: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum WgrBlend {
    Opaque = 0,
    Alpha = 1,
    Additive = 2,
    // Per-poly shadow darken: color = dst*(1-srcA). The fragment outputs black
    // with alpha = shadow strength.
    Shadow = 3,
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

// Depth-buffer interaction for a batch
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum WgrDepthMode {
    None = 0,
    Test = 1,
    TestWrite = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrDraw2DBatch {
    pub texture_id: u64,
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub blend: WgrBlend,
    pub sampler: WgrSampler2D,
    pub depth: u32,
}

// Object-space mesh vertex; matches the engine's SVertex (pos, normal, uv).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrMeshVertex {
    pub pos: WgrVec3,
    pub norm: WgrVec3,
    pub uv: WgrVec2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrDraw3D {
    pub mesh: u64,
    pub index_begin: u32,
    pub index_count: u32,
    pub texture_id: u64,
    pub world: WgrMat4,
    pub blend: WgrBlend,
    pub sampler: WgrSampler2D,
    pub camera: u32,
    // Skinning: index of this draw's 128-matrix palette block in WgrFrame.palette
    // (block b spans matrices [b*128 .. b*128+128)). NO_PALETTE = not skinned.
    pub palette_slot: u32,
    pub depth: WgrDepthMode,
    // Alpha-test cutout threshold in [0,1]; a fragment is discarded when its
    // sampled alpha is below this. 0 disables the test.
    pub alpha_ref: f32,
    pub flags: u32,
    pub _pad: u32,
}

pub const NO_PALETTE: u32 = 0xFFFF_FFFF;

// Bits for WgrDraw3D::flags (mirror WgrDraw3DFlags in wgpu_renderer.hpp).
pub const DRAW3D_ON_SURFACE: u32 = 1;
// ZBias overlay level (1..3) in bits 8-9.
pub const DRAW3D_ZBIAS_SHIFT: u32 = 8;
pub const DRAW3D_ZBIAS_MASK: u32 = 0x300;

// Frame-global scalars carried in the camera UBO (no room for a 5th bind group).
// Distinct concerns (distance fog, shadow darkening) sharing the ride.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrFrameParams {
    pub fog_start: f32,
    pub fog_inv_range: f32,
    pub fog_enabled: f32, // 0 = off, 1 = on
    pub shadow_strength: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrCamera {
    pub proj: WgrMat4,
    pub view: WgrMat4,
    // fog_color = rgb + pad
    pub fog_color: WgrVec4,
    pub params: WgrFrameParams,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WgrCmdKind {
    Draw2D = 0,
    Draw3D = 1,
    ClearDepth = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrCmd {
    pub kind: u32,
    pub arg: u32,
}

#[repr(C)]
pub struct WgrFrame {
    pub clear: WgrVec4,
    pub fog_color: WgrVec3,
    pub cameras: WgrSlice<WgrCamera>,
    pub draws3d: WgrSlice<WgrDraw3D>,
    pub verts: WgrSlice<WgrVertex2D>,
    pub batches: WgrSlice<WgrDraw2DBatch>,
    pub cmds: WgrSlice<WgrCmd>,
    // Bone-matrix pool for skinned draws: one 128-matrix block per palette slot,
    // world already pre-multiplied in (palette[i] = world * boneMatrix[i]). Length is a
    // multiple of 128.
    pub palette: WgrSlice<WgrMat4>,
}

// Layouts must match wgpu_renderer.hpp exactly (the C++ side static_asserts the same).
const _: () = assert!(std::mem::size_of::<WgrVertex2D>() == 32);
const _: () = assert!(std::mem::size_of::<WgrDraw2DBatch>() == 32);
const _: () = assert!(std::mem::size_of::<WgrMeshVertex>() == 32);
const _: () = assert!(std::mem::size_of::<WgrDraw3D>() == 120);
const _: () = assert!(std::mem::size_of::<WgrFrameParams>() == 16);
const _: () = assert!(std::mem::size_of::<WgrCamera>() == 160);
const _: () = assert!(std::mem::size_of::<WgrCmd>() == 8);
const _: () = assert!(std::mem::size_of::<WgrSlice<WgrCamera>>() == 16);
const _: () = assert!(std::mem::size_of::<WgrFrame>() == 128);

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
            Some(l) => LogSink {
                cb: l.log,
                user: l.user,
            },
            None => LogSink::none(),
        };
        match Renderer::new(desc, sink) {
            Ok(renderer) => {
                sink.log(log_level::INFO, "wgpu renderer created");
                Box::into_raw(Box::new(renderer))
            }
            Err(e) => {
                sink.log(
                    log_level::ERROR,
                    &format!("wgpu renderer creation failed: {e}"),
                );
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
/// `renderer` must be live; `verts`/`indices` must each be a valid slice (data
/// valid for its length, or null with length 0; 0 is returned if either empty).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_mesh_create(
    renderer: *mut WgrRenderer,
    verts: WgrSlice<WgrMeshVertex>,
    indices: WgrSlice<u16>,
) -> u64 {
    if renderer.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let verts = unsafe { verts.as_slice() };
        let indices = unsafe { indices.as_slice() };
        renderer.mesh_create(verts, indices)
    }))
    .unwrap_or(0)
}

/// # Safety
/// `renderer` must be live; `verts` must be a valid slice (its data valid for its
/// length, or null with length 0). `id` must be a handle returned by
/// `wgr_mesh_create` (unknown handles are ignored).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_mesh_update(
    renderer: *mut WgrRenderer,
    id: u64,
    verts: WgrSlice<WgrMeshVertex>,
) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let verts = unsafe { verts.as_slice() };
        renderer.mesh_update(id, verts);
    }));
}

/// Attach per-vertex skinning data to an existing mesh: 4 bone indices and 4
/// quantised weights per vertex (each `4 * vert_count` bytes). Weights are
/// `Unorm8x4` (0..255 -> 0..1) and should sum to ~1 per vertex.
///
/// # Safety
/// `renderer` must be live; `bones` and `weights` must each be a valid slice of
/// `4 * vert_count` bytes (data valid for its length, or null with length 0).
/// `id` must be a `wgr_mesh_create` handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_mesh_set_skin(
    renderer: *mut WgrRenderer,
    id: u64,
    bones: WgrSlice<u8>,
    weights: WgrSlice<u8>,
) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let bones = unsafe { bones.as_slice() };
        let weights = unsafe { weights.as_slice() };
        renderer.mesh_set_skin(id, bones, weights);
    }));
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
/// `renderer` and `frame` must be live pointers. Each slice in `*frame` must be
/// valid for its `len` (or null with len 0). Indices carried by `frame.cmds` /
/// `frame.draws3d` (batch, draw, camera) must be in range for their slices.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_render_frame(
    renderer: *mut WgrRenderer,
    frame: *const WgrFrame,
) -> i32 {
    if renderer.is_null() || frame.is_null() {
        return -1;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let frame = unsafe { &*frame };
        let cameras = unsafe { frame.cameras.as_slice() };
        let draws3d = unsafe { frame.draws3d.as_slice() };
        let verts = unsafe { frame.verts.as_slice() };
        let batches = unsafe { frame.batches.as_slice() };
        let cmds = unsafe { frame.cmds.as_slice() };
        let palette = unsafe { frame.palette.as_slice() };
        match renderer.render_frame(
            frame.clear,
            frame.fog_color.to_array(),
            cameras,
            draws3d,
            verts,
            batches,
            cmds,
            palette,
        ) {
            Ok(()) => 0,
            Err(e) => {
                renderer
                    .log
                    .log(log_level::ERROR, &format!("render_frame: {e}"));
                -2
            }
        }
    }))
    .unwrap_or(-3)
}
