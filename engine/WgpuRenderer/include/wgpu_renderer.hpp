/*
 * wgpu_renderer.hpp — C++ interface to the WGPU graphics backend.
 *
 * While this file contains C++ features, the actual exported symbols are all C ABI (extern "C"),
 * which the Rust side can implement using #[no_mangle] and #[repr(C)].
 */
#ifndef WGPU_RENDERER_HPP
#define WGPU_RENDERER_HPP

#include <cstdint>

#if defined(_WIN32) && !defined(WGR_STATIC)
  #define WGR_API __declspec(dllimport)
#else
  #define WGR_API
#endif

struct WgrRenderer;

// --- Math + handle aliases ---------------------------------------------------

struct WgrVec2
{
    float x, y;
};
struct WgrVec3
{
    float x, y, z;
};
struct WgrVec4
{
    float x, y, z, w;
};
struct WgrMat4
{
    float m[16]; // column-major
};

using WgrRgba8 = uint32_t;   // packed 0xAARRGGBB (engine PackedColor order)
using WgrTexture = uint64_t; // handle from wgr_texture_create; 0 = built-in white fallback
using WgrMesh = uint64_t;    // handle from wgr_mesh_create

template <typename T>
concept ContiguousContainer = requires(const T& c) {
    c.data();
    c.size();
};

template <typename T>
struct WgrSlice
{
    const T* data = nullptr;
    uint32_t length = 0;

    WgrSlice() = default;
    WgrSlice(const T* ptr, uint32_t count) : data(ptr), length(count) {}

    template <typename Container>
        requires ContiguousContainer<Container>
    WgrSlice(const Container& c) : data(c.data()), length(static_cast<uint32_t>(c.size())) { }
};

// --- Enums -------------------------------------------------------------------

/* Selects how WgrSurfaceDesc.window / .display are interpreted. */
enum WgrPlatform : int32_t
{
    WGR_PLATFORM_WIN32 = 0,   // window = HWND,         display unused
    WGR_PLATFORM_XLIB = 1,    // window = Window (XID), display = Display*
    WGR_PLATFORM_WAYLAND = 2  // window = wl_surface*,  display = wl_display*
};

enum WgrLogLevel : int32_t
{
    WGR_LOG_TRACE = 0,
    WGR_LOG_DEBUG = 1,
    WGR_LOG_INFO = 2,
    WGR_LOG_WARN = 3,
    WGR_LOG_ERROR = 4
};

enum WgrTextureFormat : int32_t
{
    WGR_TEXTURE_RGBA8 = 0,
    WGR_TEXTURE_BC1 = 1, // DXT1
    WGR_TEXTURE_BC2 = 2, // DXT3
    WGR_TEXTURE_BC3 = 3  // DXT5
};

enum WgrBlend : uint32_t
{
    WGR_BLEND_OPAQUE = 0,
    WGR_BLEND_ALPHA = 1,
    WGR_BLEND_ADDITIVE = 2,
    WGR_BLEND_SHADOW = 3
};

/* Depth-buffer interaction for a 2D/screen batch. Plain 2D and depth-disabled
 * meshes (sky: NoZBuf) use NONE; transparent / NoZWrite meshes test but don't
 * write; opaque pre-projected meshes (the laptop) test and write. */
enum WgrDepthMode : uint32_t
{
    WGR_DEPTH_NONE = 0,      // no test, no write
    WGR_DEPTH_TEST = 1,      // test (LessEqual), no write
    WGR_DEPTH_TEST_WRITE = 2 // test (LessEqual) + write
};

/* Selects what a WgrCmd does when the frame's command stream is replayed. */
enum WgrCmdKind : uint32_t
{
    WGR_CMD_DRAW_2D = 0,       // arg = index into WgrFrame.batches (its WgrDepthMode picks depth state)
    WGR_CMD_DRAW_3D = 1,       // arg = index into WgrFrame.draws3d
    WGR_CMD_CLEAR_DEPTH = 2,   // arg unused; starts a new depth-cleared segment
    WGR_CMD_DRAW_TERRAIN = 3   // arg = index into WgrFrame.terrain_batches
};

// --- Surface / logging -------------------------------------------------------

struct WgrSurfaceDesc
{
    WgrPlatform platform;
    void* window;
    void* display;
    uint32_t width;
    uint32_t height;
};

struct WgrLogCallbacks
{
    /* `log` may be NULL; `message` is only valid for the duration of the call. */
    void (*log)(int32_t level, const char* message, void* user);
    void* user;
};

// --- Vertices ----------------------------------------------------------------

/* One screen-space vertex. `pos.x`/`pos.y` are window pixels (origin top-left),
 * `pos.z` the depth, `rhw` the reciprocal clip-w (perspective-correct interp),
 * `fog` the fog blend factor (1 = keep colour, 0 = full fog). Plain 2D uses
 * pos.z=0, rhw=1, fog=1. `color` is packed 0xAARRGGBB. */
struct WgrVertex2D
{
    WgrVec3 pos;
    float rhw;
    float fog;
    WgrVec2 uv;
    WgrRgba8 color;
};

/* One object-space mesh vertex; matches the engine's SVertex (pos, normal, uv). */
struct WgrMeshVertex
{
    WgrVec3 pos;
    WgrVec3 normal;
    WgrVec2 uv;
};

// --- Draw records ------------------------------------------------------------

/* A contiguous run of triangle-list vertices sharing one texture + blend + depth
 * mode. `texture_id` 0 selects the built-in 1x1 white texture. */
struct WgrDraw2DBatch
{
    WgrTexture texture_id;
    uint32_t first_vertex; // index into WgrFrame.verts
    uint32_t vertex_count; // multiple of 3
    WgrBlend blend;
    uint32_t sampler; // bits: point<<2 | clampV<<1 | clampU
    WgrDepthMode depth;
};

/* Sentinel for WgrDraw3D::palette_slot: this draw is not skinned. */
#define WGR_NO_PALETTE 0xFFFFFFFFu

/* Capacity of the frame-global light store (WgrFrame::lights). Must match
 * MAX_LIGHTS in rust/src/gfx3d/mod.rs. The renderer clamps to this. */
#define WGR_MAX_LIGHTS 256

/* Bits for WgrDraw3D::flags. */
enum WgrDraw3DFlags : uint32_t
{
    /* Road / decal / footprint overlay: pull the draw toward the camera with a
     * polygon-offset (mirrors GL33's SetPolygonOffsetForDecals on OnSurface
     * routing) so it wins the depth test against the coplanar terrain. */
    WGR_DRAW3D_ON_SURFACE = 1,

    /* ZBias overlay level (1..3) in bits 8-9, for non-OnSurface geometry that the
     * engine biased via SetBias(level*5) (e.g. traffic-sign overlay faces). Gets a
     * stronger, level-scaled polygon-offset than a plain surface decal. */
    WGR_DRAW3D_ZBIAS_SHIFT = 8,
    WGR_DRAW3D_ZBIAS_MASK = 0x300
};

/* Matrices per palette block (the engine's own bone-palette cap). Each skinned
 * draw's palette occupies this many matrices in WgrFrame.palette. */
#define WGR_PALETTE_SIZE 128

/* A section [index_begin, index_begin+index_count) of `mesh`, textured with
 * `texture_id` (0 = built-in white), transformed by the camera-relative `world`
 * matrix. `camera` indexes WgrFrame.cameras. For skinned draws, `palette_slot`
 * indexes a 128-matrix block in WgrFrame.palette (world pre-multiplied in) and `world`
 * is ignored; WGR_NO_PALETTE = not skinned (use `world`). */
struct WgrDraw3D
{
    WgrMesh mesh;
    uint32_t index_begin;
    uint32_t index_count;
    WgrTexture texture_id;
    WgrMat4 world;
    WgrBlend blend;
    uint32_t sampler;
    uint32_t camera;
    uint32_t palette_slot;
    WgrDepthMode depth;
    /* Alpha-test cutout threshold in [0,1]: a fragment is discarded when its
     * sampled alpha is below this. 0 disables the test (nothing discarded).
     * Mirrors GL33's per-draw alphaRef (IsAlpha ~1/255, IsTransparent 0xC0). */
    float alpha_ref;
    uint32_t flags; // WgrDraw3DFlags
    uint32_t _pad;
    /* Per-draw material lighting, folded exactly like GL33's
     * UploadVSMaterialConstants: raw MainLight diffuse/ambient x material, with
     * the sun-enable already multiplied into the sun terms (emissive shows
     * regardless). The lit shader computes emissive + sun_ambient +
     * sun_diffuse * N.L, clamps to [0,1], then multiplies the texture. Only rgb
     * is read; the w lanes ride along for 16-byte std140 alignment. */
    WgrVec4 mat_emissive;
    WgrVec4 mat_sun_ambient;
    WgrVec4 mat_sun_diffuse;
    /* Material modulation for the frame-global point/spot lights (GL33's matDif /
     * matAmb before the per-light colour): raw material diffuse/ambient (eye
     * accommodation already in, night NOT — that rides the light colour). rgb. */
    WgrVec4 mat_light_diffuse;
    WgrVec4 mat_light_ambient;
    /* Sun-only Blinn-Phong specular highlight, folded like GL33's c18: rgb = raw
     * sun diffuse x material specular (sun-enable folded in, so 0 when the sun is
     * off), w = specular power. The lit shader adds rgb * pow(N.H, max(w,1))
     * per-fragment when w > 0; w <= 0 means the material has no highlight. */
    WgrVec4 mat_specular;
};

/* One frame-global point or spot light, shared by every 3D draw + terrain (bound
 * as a group-0 storage buffer). Position is ABSOLUTE world space (not
 * camera-relative like the geometry) so one upload serves every camera; the
 * shader reconstructs the camera-relative offset via the frame's cam_pos.
 * Colours are pre-scaled by the sun's NightEffect on the CPU (fade out by day,
 * matching GL33's night-only local lights). Mirrors GL33's per-draw VS lights. */
struct WgrLight
{
    WgrVec4 pos;     /* xyz = world-absolute position, w = start-attenuation distance */
    WgrVec4 diffuse; /* rgb = diffuse * nightEffect */
    WgrVec4 ambient; /* rgb = ambient * nightEffect */
    WgrVec4 dir;     /* xyz = beam direction (spot), w = isSpot (1) else 0 */
};

/* Frame-global scalars carried in the camera UBO so the 3D shader can read them
 * without a 5th bind group (wgpu's default maxBindGroups is 4). Distinct concerns
 * (distance fog, shadow darkening) that happen to share this ride for that reason.
 * shadow_strength = GetShadowFactor()/256, read by WGR_BLEND_SHADOW draws. */
struct WgrFrameParams
{
    float fog_start;
    float fog_inv_range;
    float fog_enabled; // 0 = off, 1 = on
    float shadow_strength;
};

/* Per-camera cascaded-shadow sampling block, read by the lit 3D shaders. All
 * zeros (ctl.x = cascade count = 0 -> disabled) when shadow maps are off or for
 * UI/screen cameras; the depth pass itself is driven by WgrShadowPass, not this. */
struct WgrCameraShadow
{
    WgrMat4 cascade_vp[4]; // camera-relative light view-projections (0..1 NDC z)
    WgrVec4 splits;        // frustum tiers: far eye-depth per tier
    WgrVec4 omni_radius;   // omni tiers: camera-distance radius (0 = frustum tier)
    WgrVec4 ctl;           // {count, omni_count, fade_range, bias_const}
    WgrVec4 ctlb;          // {texel_size (1/res), darkness, normal_offset_scale, pcf}
    WgrVec4 cam_fwd;       // xyz = camera forward (eye-depth cascade select)
    WgrVec4 sun_dir;       // xyz = sun travel direction (normal-offset bias)
};

/* A view + projection pair, plus the frame-global params (see WgrFrameParams).
 * fog_color = rgb (+pad). */
struct WgrCamera
{
    WgrMat4 proj;
    WgrMat4 view;
    WgrVec4 fog_color;
    WgrFrameParams params;
    WgrCameraShadow shadow;
    /* World-space camera position. `view` has no translation (geometry is
     * camera-relative); terrain uses this to sample the world-space heightmap. */
    WgrVec4 cam_pos;
    /* Sun light for GPU-lit paths (terrain): rgb, pre-multiplied by the eye
     * accommodation — DoLightingColorized's DiffusePrecalc/AmbientPrecalc for a
     * white material. */
    WgrVec4 sun_diffuse;
    WgrVec4 sun_ambient;
    /* xyz = normalized MainLight()->Direction(): the sun's light TRAVEL
     * direction (downward by day, upward while the sun is below the horizon —
     * which is what keeps night terrain ambient-only). Same convention as
     * GL33's sunDir constant; shaders dot the normal with its negation. Valid
     * every frame, unlike the shadow block's sun_dir. */
    WgrVec4 sun_dir_world;
};

/* One shadow caster for the cascade depth passes: a section run
 * [index_begin, index_begin+index_count) of `mesh`, transformed by the
 * camera-relative `world` (or, when `palette_slot` is valid, GPU-skinned by that
 * palette block exactly like a WgrDraw3D). `alpha_ref` > 0 alpha-tests the
 * caster texture so cutout foliage casts a leaf silhouette instead of a blob. */
struct WgrShadowCaster
{
    WgrMesh mesh;
    uint32_t index_begin;
    uint32_t index_count;
    WgrMat4 world;
    WgrTexture texture_id; // sampled only when alpha_ref > 0; 0 = built-in white
    uint32_t palette_slot; // WGR_NO_PALETTE = rigid
    float alpha_ref;       // 0 = solid caster; > 0 = discard below (cutout)
    uint32_t sampler;
    uint32_t cascade_mask; // bit c set = render into cascade c
};

/* Cascade depth-pass parameters for one frame. The renderer draws
 * WgrFrame.shadow_casters into a `count`-layer depth array from these
 * camera-relative light view-projections before replaying the frame's command
 * stream. count = 0 disables the pass (and keeps last frame's map unused). */
struct WgrShadowPass
{
    uint32_t count; // cascade count (1..4); 0 = no shadow pass this frame
    uint32_t omni_count;
    uint32_t resolution; // depth-map side length per cascade
    uint32_t _pad;
    WgrMat4 light_vp[4]; // camera-relative light view-projections (0..1 NDC z)
};

/* One entry in the frame's submission-ordered command stream. */
struct WgrCmd
{
    WgrCmdKind kind;
    uint32_t arg;
};

// --- Terrain (GPU heightmap) -------------------------------------------------

/* Per-map terrain parameters, uploaded once with the heightmap. The heightmap is
 * an hm_width x hm_height R32Float texture of world heights sampled in the vertex
 * shader; `terrain_grid` is the world spacing between adjacent heightmap texels,
 * `land_grid` the coarser texture-cell spacing, `world_origin` the world-space xz
 * of texel (0,0). `data_scale` is currently unused (heights arrive in metres). */
struct WgrTerrainParams
{
    WgrVec2 world_origin;
    float land_grid;
    float terrain_grid;
    uint32_t hm_width;
    uint32_t hm_height;
    uint32_t land_range; // land-cell count per axis
    float data_scale;
};

/* One terrain node instance: the shared grid mesh placed at world-xz `origin`,
 * covering `size` x `size` world units, at level `lod`. `morph_start`/`morph_end`
 * are the camera-distance band over which the grid morphs toward its coarser parent. */
struct WgrTerrainNode
{
    WgrVec2 origin;
    float size;
    uint32_t lod;
    float morph_start;
    float morph_end;
};

/* A run [first_node, first_node+node_count) of WgrFrame.terrain_nodes drawn with
 * the shared grid mesh, transformed by camera `camera` (indexes WgrFrame.cameras). */
struct WgrTerrainBatch
{
    uint32_t first_node;
    uint32_t node_count;
    uint32_t camera;
    uint32_t _pad;
};

/* Overlay (dev panel / ImGui) vertex: framebuffer pixels, top-left origin.
 * `color` is RGBA with R in the low byte (ImGui packing, NOT WgrRgba8). */
struct WgrOverlayVertex
{
    WgrVec2 pos;
    WgrVec2 uv;
    uint32_t color;
};

/* One scissored overlay draw: `index_count` indices from
 * WgrFrame.overlay_indices starting at `first_index`, offset by `base_vertex`
 * into WgrFrame.overlay_verts, clipped to `clip` = {x0, y0, x1, y1} pixels. */
struct WgrOverlayDraw
{
    WgrVec4 clip;
    WgrTexture texture_id; // 0 = built-in white
    uint32_t first_index;
    uint32_t index_count;
    uint32_t base_vertex;
    uint32_t _pad;
};

// --- Frame -------------------------------------------------------------------

/* Everything needed to render + present one frame. The renderer clears to
 * `clear` (+depth), then replays `cmds` in submission order: each 2D batch and
 * 3D draw renders interleaved exactly as recorded, so 3D UI elements land
 * between their 2D background and foreground. WGR_CMD_CLEAR_DEPTH starts a new
 * segment with a freshly cleared depth buffer (colour preserved). 3D draws are
 * depth-tested and transformed by `cameras[draw.camera]`. `fog_color` is what
 * each vertex's `fog` blends toward. Any slice may be empty. */
struct WgrFrame
{
    WgrVec4 clear;
    WgrVec3 fog_color;

    WgrSlice<WgrCamera> cameras;
    WgrSlice<WgrDraw3D> draws3d;
    WgrSlice<WgrVertex2D> verts;
    WgrSlice<WgrDraw2DBatch> batches;
    WgrSlice<WgrCmd> cmds;
    /* Bone-matrix pool for skinned draws: one 128-matrix block per palette slot,
     * world already pre-multiplied in (palette[i] = world * boneMatrix[i]). Length is a
     * multiple of 128. Empty if no skinned draws. */
    WgrSlice<WgrMat4> palette;

    /* Cascaded-shadow depth pass: rendered before the command stream when
     * shadow.count > 0 and shadow_casters is non-empty. */
    WgrShadowPass shadow;
    WgrSlice<WgrShadowCaster> shadow_casters;

    /* Overlay (dev panel): alpha-blended over the finished frame, no depth. */
    WgrSlice<WgrOverlayVertex> overlay_verts;
    WgrSlice<uint16_t> overlay_indices;
    WgrSlice<WgrOverlayDraw> overlay_draws;

    /* GPU terrain nodes, drawn on WGR_CMD_DRAW_TERRAIN. The heightmap + ground
     * textures are uploaded separately via wgr_terrain_*. */
    WgrSlice<WgrTerrainNode> terrain_nodes;
    WgrSlice<WgrTerrainBatch> terrain_batches;

    /* Frame-global point/spot lights (<= 256), uploaded once into the group-0
     * storage buffer shared by 3D draws + terrain. The per-camera light count
     * rides in WgrCamera::cam_pos.w. */
    WgrSlice<WgrLight> lights;
};

// --- Layout guards (mirror rust/src/ffi.rs) ----------------------------------

static_assert(sizeof(WgrVec2) == 8, "WgrVec2 must be 2 floats");
static_assert(sizeof(WgrVec3) == 12, "WgrVec3 must be 3 floats");
static_assert(sizeof(WgrVec4) == 16, "WgrVec4 must be 4 floats");
static_assert(sizeof(WgrMat4) == 64, "WgrMat4 must be 16 floats");
static_assert(sizeof(WgrSlice<WgrCamera>) == 16 && alignof(WgrSlice<WgrCamera>) == 8,
              "WgrSlice must be a { pointer, u32 } with 8-byte alignment");
static_assert(sizeof(WgrBlend) == 4, "WgrBlend must be 4 bytes to match the Rust #[repr(u32)] enum");
static_assert(sizeof(WgrVertex2D) == 32, "WgrVertex2D layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrMeshVertex) == 32, "WgrMeshVertex must match the engine SVertex layout");
static_assert(sizeof(WgrDraw2DBatch) == 32, "WgrDraw2DBatch layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrDraw3D) == 216, "WgrDraw3D layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrLight) == 64, "WgrLight layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrFrameParams) == 16, "WgrFrameParams layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCameraShadow) == 352, "WgrCameraShadow layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCamera) == 576, "WgrCamera layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrShadowCaster) == 104, "WgrShadowCaster layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrShadowPass) == 272, "WgrShadowPass layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCmd) == 8, "WgrCmd layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrOverlayVertex) == 20, "WgrOverlayVertex layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrOverlayDraw) == 40, "WgrOverlayDraw layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrTerrainParams) == 32, "WgrTerrainParams layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrTerrainNode) == 24, "WgrTerrainNode layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrTerrainBatch) == 16, "WgrTerrainBatch layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrFrame) == 512, "WgrFrame layout must match the Rust #[repr(C)] struct");

// --- Functions ---------------------------------------------------------------

extern "C"
{
    WGR_API const char* wgr_version(void);

    /* Returns NULL on failure (reason reported via `log` if supplied). `log` may be NULL. */
    WGR_API WgrRenderer* wgr_create(const WgrSurfaceDesc* desc, const WgrLogCallbacks* log);

    WGR_API void wgr_destroy(WgrRenderer* renderer);
    WGR_API void wgr_resize(WgrRenderer* renderer, uint32_t width, uint32_t height);

    /* Upload a texture in `format` (WgrTextureFormat); returns a non-zero id, or
     * 0 on failure. `data` holds `mip_count` tightly packed mip levels, level i
     * sized for (max(1, width>>i), max(1, height>>i)): RGBA8 = w*h*4 per level;
     * BC* = the block-payload size (ceil(w/4)*ceil(h/4) * 8 for BC1, * 16 for
     * BC2/BC3). `byte_length` is the total. Pass WGR_TEXTURE_GEN_MIPS in `flags`
     * (RGBA8, mip_count 1 only) to generate the rest of the chain with a box
     * filter. */
    WGR_API WgrTexture wgr_texture_create(WgrRenderer* renderer, uint32_t width, uint32_t height, int32_t format,
                                          uint32_t mip_count, uint32_t flags, const uint8_t* data,
                                          uint32_t byte_length);

    constexpr uint32_t WGR_TEXTURE_GEN_MIPS = 1;

    /* Replace the pixels of an existing RGBA8 texture. */
    WGR_API void wgr_texture_update(WgrRenderer* renderer, WgrTexture id, const uint8_t* rgba, uint32_t byte_length);

    WGR_API void wgr_texture_destroy(WgrRenderer* renderer, WgrTexture id);

    /* Create a static mesh from interleaved vertices + 16-bit triangle-list
     * indices; returns a non-zero handle, or 0 on failure. */
    WGR_API WgrMesh wgr_mesh_create(WgrRenderer* renderer, WgrSlice<WgrMeshVertex> verts, WgrSlice<uint16_t> indices);

    /* Re-upload vertex data for an existing mesh (dynamic / animated shapes).
     * The topology (indices) is unchanged; the vertex count must not exceed the
     * mesh's original vertex count. */
    WGR_API void wgr_mesh_update(WgrRenderer* renderer, WgrMesh id, WgrSlice<WgrMeshVertex> verts);

    /* Attach per-vertex skinning data to a mesh: 4 bone indices + 4 quantised
     * weights per vertex (each buffer `4 * vert_count` bytes). Weights are
     * Unorm8x4 (0..255 -> 0..1) and should sum to ~1 per vertex. */
    WGR_API void wgr_mesh_set_skin(WgrRenderer* renderer, WgrMesh id, WgrSlice<uint8_t> bones,
                                   WgrSlice<uint8_t> weights);

    WGR_API void wgr_mesh_destroy(WgrRenderer* renderer, WgrMesh id);

    /* Upload (or replace) the terrain heightmap: `heights` is
     * params->hm_width * params->hm_height row-major world-height floats (row 0 =
     * texel z=0). Creates the R32Float heightmap texture + params UBO, once per
     * map load. */
    WGR_API void wgr_terrain_set_heightmap(WgrRenderer* renderer, const float* heights,
                                           const WgrTerrainParams* params);

    /* Set the terrain ground layers: `handles[i]` is the wgr_texture_create
     * handle for Landscape texture index i (0 = the built-in white fallback).
     * Layers keep their native size/format/mips; the fragment shader samples
     * them through a bindless binding_array, indexed per land cell by
     * wgr_terrain_set_index_map. At most WGR_TERRAIN_MAX_GROUND_LAYERS are
     * used; the index-map upload must clamp cell indices to the same bound. */
    WGR_API void wgr_terrain_set_ground_layers(WgrRenderer* renderer, const uint64_t* handles, uint32_t count);

    constexpr uint32_t WGR_TERRAIN_MAX_GROUND_LAYERS = 512;

    /* Upload the per-land-cell texture index map: a `width` x `height` (= land
     * range per axis) R16Uint texture where each texel's bits 0-14 are the
     * ground-layer index for that land cell (row 0 = cell z=0; index 0 = sea).
     * Bit 15 marks a clamped transition tile: its texture maps exactly once onto
     * the cell with edges extended (GL33's ClampU|ClampV) instead of tiling.
     * `indices` is width*height uint16s. */
    WGR_API void wgr_terrain_set_index_map(WgrRenderer* renderer, uint32_t width, uint32_t height,
                                           const uint16_t* indices);

    /* Upload the per-grid-point ground UV jitter map: a `width` x `height`
     * (= land range per axis) Rg8Snorm texture holding each land grid point's
     * random texture UV offset (Landscape::_random, at most +-0.7). The fragment
     * shader interpolates it bilinearly across each cell and adds it to the
     * ground tiling UV, replicating GL33's per-vertex jitter. `offsets` is
     * width*height (u, v) int8 pairs (snorm: value / 127). */
    WGR_API void wgr_terrain_set_jitter_map(WgrRenderer* renderer, uint32_t width, uint32_t height,
                                            const int8_t* offsets);

    /* Set the high-frequency detail noise texture tiled over the terrain
     * (OFP's `CfgDetailTextures.detail`) to a wgr_texture_create handle; its
     * alpha channel modulates the blended ground colour (rgb *= 2*detail.a).
     * Handle 0 is ignored (the neutral built-in stand-in stays). */
    WGR_API void wgr_terrain_set_detail_layer(WgrRenderer* renderer, WgrTexture handle);

    /* Live-tune the long-distance terrain sun-shadow sweep (heightfield self-
     * shadow, a complement to the cascade maps). `strength` scales the occlusion
     * (0 = off, 1 = physical, >1 = exaggerated for debugging); `scale` is the mask
     * supersample factor over the heightmap grid (>=1, higher = sharper edges,
     * more VRAM); `max_steps` caps the march range (steps * terrain_grid metres);
     * `penumbra_deg` is the soft-edge half-width in degrees. Changing `scale`
     * reallocates the mask; any change re-runs the amortized sweep next frame. */
    WGR_API void wgr_terrain_set_sun_shadow(WgrRenderer* renderer, float strength, uint32_t scale,
                                            uint32_t max_steps, float penumbra_deg);

    /* Render + present one frame. Returns 0 on success (incl. transient skipped
     * frames), negative on error. */
    WGR_API int32_t wgr_render_frame(WgrRenderer* renderer, const WgrFrame* frame);

    /* Read one cascade layer of the shadow depth map back as row-major floats
     * (row 0 = the top texture row). Returns the map resolution (side length),
     * or 0 when no map has been rendered / `layer` is out of range / `out_len`
     * is smaller than resolution². Debug/test hook (DumpShadowMap). */
    WGR_API uint32_t wgr_shadow_map_read(WgrRenderer* renderer, uint32_t layer, float* out, uint32_t out_len);

    /* Render `vert_count` triangle-list vertices (xyz, 3 floats each) through
     * the shadow depth pipeline with the given column-major light
     * view-projection into a scratch res*res depth map, and read it back into
     * `out` (res*res floats, row 0 = top). Returns 1 on success. Debug/test
     * hook (ShadowDepthProbe: CPU-reference parity for the depth path). */
    WGR_API int32_t wgr_shadow_depth_probe(WgrRenderer* renderer, const float* light_vp16, const float* tri_xyz,
                                           uint32_t vert_count, uint32_t res, float* out);

} // extern "C"

#endif // WGPU_RENDERER_HPP
