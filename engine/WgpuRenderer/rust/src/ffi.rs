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
    // Per-vertex terrain-conform selector (0 = rigid, 1 = ClipLandKeep, 2 = ClipLandOn),
    // read by vs_main at @location(5). Only meaningful when the draw's conform mode
    // selects the per-vertex heightmap path (individual ClipLand vegetation).
    pub conform: u32,
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
    // Per-draw material lighting, folded exactly like GL33's
    // UploadVSMaterialConstants (raw sun colour x material, sun-enable already
    // multiplied into the sun terms; emissive shows regardless). The lit shader
    // computes `emissive + sun_ambient + sun_diffuse * N.L`, clamps, x texture.
    // rgb used; the w lanes ride along for 16-byte std140 alignment.
    pub mat_emissive: WgrVec4,
    pub mat_sun_ambient: WgrVec4,
    pub mat_sun_diffuse: WgrVec4,
    // Material modulation for the frame-global point/spot lights (GL33's matDif /
    // matAmb before the per-light colour): raw material diffuse/ambient (eye
    // accommodation already in, night NOT — that rides the light colour). rgb used.
    pub mat_light_diffuse: WgrVec4,
    pub mat_light_ambient: WgrVec4,
    // Sun-only Blinn-Phong specular highlight, folded like GL33's c18: rgb = raw
    // sun diffuse x material specular (sun-enable folded in, so 0 when the sun is
    // off), w = specular power. The lit shader adds `rgb * pow(N.H, max(w,1))`
    // per-fragment when w > 0; w <= 0 means the material has no highlight.
    pub mat_specular: WgrVec4,
    // Terrain-conform plane for GPU vegetation (ForestPlain). When conform2.z (mode)
    // > 0 the vertex shader displaces this draw's vertices onto the ground exactly like
    // ForestPlain::Animate's two-triangle bilinear fit, so the shared forest mesh is
    // uploaded once undeformed instead of rewritten per instance. Zero (mode 0) for
    // every non-conformed draw. See terrain-conform-vegetation-roads-plan.
    pub conform0: WgrVec4, // inv_land_grid, -xf, -zf, bias(=BoundingCenter().y)
    pub conform1: WgrVec4, // y00, y10, d1000, d0100
    pub conform2: WgrVec4, // d1011, d0111, mode, _pad
}

// One frame-global point or spot light, shared by every 3D draw + terrain (bound
// as a group-0 storage buffer). Positions are ABSOLUTE world space (not
// camera-relative like the geometry) so a single upload serves every camera; the
// shader reconstructs the camera-relative offset via the frame's cam_pos. Colours
// are pre-scaled by the sun's NightEffect on the CPU, so they fade out by day
// (GL33's night-only local lights). Mirrors GL33's per-draw VS light constants.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrLight {
    pub pos: WgrVec4,     // xyz = world-absolute position, w = start-attenuation distance
    pub diffuse: WgrVec4, // rgb = diffuse * nightEffect
    pub ambient: WgrVec4, // rgb = ambient * nightEffect
    pub dir: WgrVec4,     // xyz = beam direction (spot), w = isSpot (1) else 0
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

// Per-camera cascaded-shadow sampling block (lit-pass side). All zeros
// (ctl.x = cascade count = 0 -> disabled) when shadow maps are off or for
// UI/screen cameras.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrCameraShadow {
    pub cascade_vp: [WgrMat4; 4],
    pub splits: WgrVec4,      // frustum tiers: far eye-depth per tier
    pub omni_radius: WgrVec4, // omni tiers: camera-distance radius (0 = frustum tier)
    pub ctl: WgrVec4,         // {count, omni_count, fade_range, bias_const}
    pub ctlb: WgrVec4,        // {texel_size (1/res), darkness, normal_offset_scale, pcf}
    pub cam_fwd: WgrVec4,     // xyz = camera forward (eye-depth cascade select)
    pub sun_dir: WgrVec4,     // xyz = sun travel direction (normal-offset bias)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrCamera {
    pub proj: WgrMat4,
    pub view: WgrMat4,
    // fog_color = rgb + pad
    pub fog_color: WgrVec4,
    pub params: WgrFrameParams,
    pub shadow: WgrCameraShadow,
    // World-space camera position (view drops its translation; geometry is
    // camera-relative). GPU terrain uses it for heightmap sampling.
    pub cam_pos: WgrVec4,
    // Sun light for GPU-lit paths (terrain): rgb, pre-multiplied by the eye
    // accommodation on the C++ side.
    pub sun_diffuse: WgrVec4,
    pub sun_ambient: WgrVec4,
    // xyz = normalized sun light TRAVEL direction (GL33's sunDir convention:
    // shaders dot the normal with its negation); valid every frame, unlike the
    // shadow block's sun_dir.
    pub sun_dir_world: WgrVec4,
}

// One shadow caster for the cascade depth passes: a section run of `mesh`,
// transformed by the camera-relative `world` (or skinned via `palette_slot`).
// alpha_ref > 0 alpha-tests the caster texture (cutout foliage silhouettes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrShadowCaster {
    pub mesh: u64,
    pub index_begin: u32,
    pub index_count: u32,
    pub world: WgrMat4,
    pub texture_id: u64,   // sampled only when alpha_ref > 0; 0 = built-in white
    pub palette_slot: u32, // NO_PALETTE = rigid
    pub alpha_ref: f32,    // 0 = solid caster; > 0 = discard below (cutout)
    pub sampler: WgrSampler2D,
    pub cascade_mask: u32, // bit c set = render into cascade c
    // Terrain-conform plane for this caster (mirrors WgrDraw3D::conform*). Mode 2
    // (conform2.z) conforms ClipLand vegetation to SurfaceY per vertex in the depth
    // shader, so the shared shadow mesh is uploaded ONCE undeformed. 0 = rigid.
    pub conform0: WgrVec4, // x = bcSurfaceY
    pub conform2: WgrVec4, // z = mode
}

// Cascade depth-pass parameters for one frame; count = 0 disables the pass.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrShadowPass {
    pub count: u32, // cascade count (1..4); 0 = no shadow pass this frame
    pub omni_count: u32,
    pub resolution: u32, // depth-map side length per cascade
    pub _pad: u32,
    pub light_vp: [WgrMat4; 4], // camera-relative light view-projections (0..1 NDC z)
    // Camera world position: casters are camera-relative, so the depth shader adds
    // this back to reconstruct absolute world xz for surface_y (terrain conform).
    pub cam_pos: WgrVec4,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WgrCmdKind {
    Draw2D = 0,
    Draw3D = 1,
    ClearDepth = 2,
    DrawTerrain = 3,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrCmd {
    pub kind: u32,
    pub arg: u32,
}

// Static per-map terrain parameters, uploaded with the heightmap. See
// wgpu_renderer.hpp for field semantics.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrTerrainParams {
    pub world_origin: WgrVec2,
    pub land_grid: f32,
    pub terrain_grid: f32,
    pub hm_width: u32,
    pub hm_height: u32,
    pub land_range: u32,
    pub data_scale: f32,
}

// One terrain node (shared grid mesh at world-xz `origin`, `size` wide, level
// `lod`). Uploaded as instance-step vertex data.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrTerrainNode {
    pub origin: WgrVec2,
    pub size: f32,
    pub lod: u32,
    pub morph_start: f32,
    pub morph_end: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrTerrainBatch {
    pub first_node: u32,
    pub node_count: u32,
    pub camera: u32,
    pub _pad: u32,
}

// Overlay (dev panel / ImGui) vertex: framebuffer pixels, top-left origin.
// `color` is RGBA with R in the low byte (ImGui packing, NOT the engine order).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WgrOverlayVertex {
    pub pos: WgrVec2,
    pub uv: WgrVec2,
    pub color: u32,
}

// One scissored overlay draw over the frame's overlay index/vertex slices.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WgrOverlayDraw {
    pub clip: WgrVec4, // {x0, y0, x1, y1} pixels
    pub texture_id: u64,
    pub first_index: u32,
    pub index_count: u32,
    pub base_vertex: u32,
    pub _pad: u32,
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
    // Cascaded-shadow depth pass: rendered before the command stream when
    // shadow.count > 0 and shadow_casters is non-empty.
    pub shadow: WgrShadowPass,
    pub shadow_casters: WgrSlice<WgrShadowCaster>,
    // Overlay (dev panel): alpha-blended over the finished frame, no depth.
    pub overlay_verts: WgrSlice<WgrOverlayVertex>,
    pub overlay_indices: WgrSlice<u16>,
    pub overlay_draws: WgrSlice<WgrOverlayDraw>,
    // GPU terrain nodes, drawn on WGR_CMD_DRAW_TERRAIN.
    pub terrain_nodes: WgrSlice<WgrTerrainNode>,
    pub terrain_batches: WgrSlice<WgrTerrainBatch>,
    // Frame-global point/spot lights (<= 256), uploaded once into the group-0
    // storage buffer shared by 3D draws + terrain. The per-camera light count
    // rides in WgrCamera::cam_pos.w.
    pub lights: WgrSlice<WgrLight>,
}

// Layouts must match wgpu_renderer.hpp exactly (the C++ side static_asserts the same).
const _: () = assert!(std::mem::size_of::<WgrVertex2D>() == 32);
const _: () = assert!(std::mem::size_of::<WgrDraw2DBatch>() == 32);
const _: () = assert!(std::mem::size_of::<WgrMeshVertex>() == 36);
const _: () = assert!(std::mem::size_of::<WgrDraw3D>() == 264);
const _: () = assert!(std::mem::size_of::<WgrLight>() == 64);
const _: () = assert!(std::mem::size_of::<WgrFrameParams>() == 16);
const _: () = assert!(std::mem::size_of::<WgrCameraShadow>() == 352);
const _: () = assert!(std::mem::size_of::<WgrCamera>() == 576);
const _: () = assert!(std::mem::size_of::<WgrShadowCaster>() == 136);
const _: () = assert!(std::mem::size_of::<WgrShadowPass>() == 288);
const _: () = assert!(std::mem::size_of::<WgrCmd>() == 8);
const _: () = assert!(std::mem::size_of::<WgrOverlayVertex>() == 20);
const _: () = assert!(std::mem::size_of::<WgrOverlayDraw>() == 40);
const _: () = assert!(std::mem::size_of::<WgrTerrainParams>() == 32);
const _: () = assert!(std::mem::size_of::<WgrTerrainNode>() == 24);
const _: () = assert!(std::mem::size_of::<WgrTerrainBatch>() == 16);
const _: () = assert!(std::mem::size_of::<WgrSlice<WgrCamera>>() == 16);
const _: () = assert!(std::mem::size_of::<WgrFrame>() == 528);

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

/// Flag for wgr_texture_create: generate the rest of the mip chain from level 0
/// with a box filter (RGBA8 with mip_count 1 only). Must match
/// WGR_TEXTURE_GEN_MIPS.
pub const TEXTURE_GEN_MIPS: u32 = 1;

/// # Safety
/// `renderer` must be live; `data` must point to at least `byte_len` bytes
/// (holding `mip_count` tightly packed mip levels), or be null (in which case 0
/// is returned).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_texture_create(
    renderer: *mut WgrRenderer,
    width: u32,
    height: u32,
    format: i32,
    mip_count: u32,
    flags: u32,
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
        renderer.texture_create(
            width,
            height,
            fmt,
            mip_count,
            flags & TEXTURE_GEN_MIPS != 0,
            slice,
        )
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

/// Upload (or replace) the terrain heightmap + params. See wgpu_renderer.hpp.
///
/// # Safety
/// `renderer` must be live; `params` must point to a valid `WgrTerrainParams`;
/// `heights` must point to at least `hm_width * hm_height` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_terrain_set_heightmap(
    renderer: *mut WgrRenderer,
    heights: *const f32,
    params: *const WgrTerrainParams,
) {
    if renderer.is_null() || heights.is_null() || params.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let params = unsafe { *params };
        let count = params.hm_width as usize * params.hm_height as usize;
        let heights = unsafe { std::slice::from_raw_parts(heights, count) };
        renderer.terrain_set_heightmap(heights, params);
    }));
}

/// Set the terrain ground layers as a list of wgr_texture_create handles (one
/// per Landscape texture index). See wgpu_renderer.hpp.
///
/// # Safety
/// `renderer` must be live; `handles` must point to at least `count` `u64`s, or
/// be null (in which case the call is ignored).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_terrain_set_ground_layers(
    renderer: *mut WgrRenderer,
    handles: *const u64,
    count: u32,
) {
    if renderer.is_null() || handles.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let slice = unsafe { std::slice::from_raw_parts(handles, count as usize) };
        renderer.terrain_set_ground_layers(slice);
    }));
}

/// Upload the per-land-cell texture index map (R16Uint). See wgpu_renderer.hpp.
///
/// # Safety
/// `renderer` must be live; `indices` must point to at least `width * height`
/// `u16`s, or be null (in which case the call is ignored).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_terrain_set_index_map(
    renderer: *mut WgrRenderer,
    width: u32,
    height: u32,
    indices: *const u16,
) {
    if renderer.is_null() || indices.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let count = width as usize * height as usize;
        let slice = unsafe { std::slice::from_raw_parts(indices, count) };
        renderer.terrain_set_index_map(width, height, slice);
    }));
}

/// Upload the per-grid-point ground UV jitter map (Rg8Snorm). See
/// wgpu_renderer.hpp.
///
/// # Safety
/// `renderer` must be live; `offsets` must point to at least
/// `2 * width * height` `i8`s, or be null (in which case the call is ignored).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_terrain_set_jitter_map(
    renderer: *mut WgrRenderer,
    width: u32,
    height: u32,
    offsets: *const i8,
) {
    if renderer.is_null() || offsets.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let count = 2 * width as usize * height as usize;
        let slice = unsafe { std::slice::from_raw_parts(offsets, count) };
        renderer.terrain_set_jitter_map(width, height, slice);
    }));
}

/// Live-tune the long-distance terrain sun-shadow sweep (heightfield self-shadow).
/// `strength` scales the occlusion (0 = off, 1 = physical, >1 = exaggerated for
/// debugging); `scale` is the mask supersample factor over the heightmap;
/// `max_steps` caps the march range (steps * terrain_grid); `penumbra_deg` is the
/// soft-edge half-width in degrees. Changing `scale` reallocates the mask; any
/// change re-runs the (amortized) sweep on the next frame. See wgpu_renderer.hpp.
///
/// # Safety
/// `renderer` must be a live pointer from `wgr_create`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_terrain_set_sun_shadow(
    renderer: *mut WgrRenderer,
    strength: f32,
    scale: u32,
    max_steps: u32,
    penumbra_deg: f32,
) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        unsafe { &mut *renderer }.terrain_set_sun_shadow(strength, scale, max_steps, penumbra_deg);
    }));
}

/// Set the terrain detail noise texture to a wgr_texture_create handle. See
/// wgpu_renderer.hpp.
///
/// # Safety
/// `renderer` must be a live pointer from `wgr_create`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_terrain_set_detail_layer(renderer: *mut WgrRenderer, handle: u64) {
    if renderer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        unsafe { &mut *renderer }.terrain_set_detail_layer(handle);
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
        let shadow_casters = unsafe { frame.shadow_casters.as_slice() };
        let overlay_verts = unsafe { frame.overlay_verts.as_slice() };
        let overlay_indices = unsafe { frame.overlay_indices.as_slice() };
        let overlay_draws = unsafe { frame.overlay_draws.as_slice() };
        let terrain_nodes = unsafe { frame.terrain_nodes.as_slice() };
        let terrain_batches = unsafe { frame.terrain_batches.as_slice() };
        let lights = unsafe { frame.lights.as_slice() };
        match renderer.render_frame(
            frame.clear,
            frame.fog_color.to_array(),
            cameras,
            draws3d,
            verts,
            batches,
            cmds,
            palette,
            &frame.shadow,
            shadow_casters,
            overlay_verts,
            overlay_indices,
            overlay_draws,
            terrain_nodes,
            terrain_batches,
            lights,
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

/// Read one cascade layer of the shadow depth map back as row-major floats
/// (row 0 = top). Returns the map resolution (side length), or 0 when no map
/// exists / `layer` is out of range / `out_len` is too small.
///
/// # Safety
/// `renderer` must be live; `out` must point to at least `out_len` floats, or
/// be null (in which case 0 is returned).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_shadow_map_read(
    renderer: *mut WgrRenderer,
    layer: u32,
    out: *mut f32,
    out_len: u32,
) -> u32 {
    if renderer.is_null() || out.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let out = unsafe { std::slice::from_raw_parts_mut(out, out_len as usize) };
        renderer.shadow_map_read(layer, out)
    }))
    .unwrap_or(0)
}

/// Render a triangle soup through the shadow depth pipeline into a scratch
/// res*res map and read it back (row 0 = top). Returns 1 on success.
///
/// # Safety
/// `renderer` must be live; `light_vp16` must point to 16 floats, `tri_xyz` to
/// `3 * vert_count` floats, and `out` to `res * res` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wgr_shadow_depth_probe(
    renderer: *mut WgrRenderer,
    light_vp16: *const f32,
    tri_xyz: *const f32,
    vert_count: u32,
    res: u32,
    out: *mut f32,
) -> i32 {
    if renderer.is_null() || light_vp16.is_null() || tri_xyz.is_null() || out.is_null() || res == 0
    {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = unsafe { &mut *renderer };
        let vp: &[f32; 16] = unsafe { &*(light_vp16 as *const [f32; 16]) };
        let verts = unsafe { std::slice::from_raw_parts(tri_xyz, vert_count as usize * 3) };
        let out = unsafe { std::slice::from_raw_parts_mut(out, (res * res) as usize) };
        renderer.shadow_depth_probe(vp, verts, res, out) as i32
    }))
    .unwrap_or(0)
}
