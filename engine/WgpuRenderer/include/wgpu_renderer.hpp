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
    WGR_BLEND_ADDITIVE = 2
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
    WGR_CMD_DRAW_2D = 0,     // arg = index into WgrFrame.batches (its WgrDepthMode picks depth state)
    WGR_CMD_DRAW_3D = 1,     // arg = index into WgrFrame.draws3d
    WGR_CMD_CLEAR_DEPTH = 2  // arg unused; starts a new depth-cleared segment
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
    uint32_t _pad;
};

/* A view + projection pair, plus the frame's fog state. Fog is frame-global but
 * carried here so the 3D shader reads it without a 5th bind group (wgpu's default
 * maxBindGroups is 4). fog_color = rgb (+pad); fog_params = {start, inv_range,
 * enabled, pad}. */
struct WgrCamera
{
    WgrMat4 proj;
    WgrMat4 view;
    WgrVec4 fog_color;
    WgrVec4 fog_params;
};

/* One entry in the frame's submission-ordered command stream. */
struct WgrCmd
{
    WgrCmdKind kind;
    uint32_t arg;
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
static_assert(sizeof(WgrDraw3D) == 112, "WgrDraw3D layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCamera) == 160, "WgrCamera layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrCmd) == 8, "WgrCmd layout must match the Rust #[repr(C)] struct");
static_assert(sizeof(WgrFrame) == 128, "WgrFrame layout must match the Rust #[repr(C)] struct");

// --- Functions ---------------------------------------------------------------

extern "C"
{
    WGR_API const char* wgr_version(void);

    /* Returns NULL on failure (reason reported via `log` if supplied). `log` may be NULL. */
    WGR_API WgrRenderer* wgr_create(const WgrSurfaceDesc* desc, const WgrLogCallbacks* log);

    WGR_API void wgr_destroy(WgrRenderer* renderer);
    WGR_API void wgr_resize(WgrRenderer* renderer, uint32_t width, uint32_t height);

    /* Upload a single-level texture in `format` (WgrTextureFormat); returns a
     * non-zero id, or 0 on failure. `byte_length` must match the format: RGBA8 =
     * width*height*4; BC* = the block-payload size (ceil(w/4)*ceil(h/4) * 8 for
     * BC1, * 16 for BC2/BC3). */
    WGR_API WgrTexture wgr_texture_create(WgrRenderer* renderer, uint32_t width, uint32_t height, int32_t format,
                                          const uint8_t* data, uint32_t byte_length);

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

    /* Render + present one frame. Returns 0 on success (incl. transient skipped
     * frames), negative on error. */
    WGR_API int32_t wgr_render_frame(WgrRenderer* renderer, const WgrFrame* frame);

} // extern "C"

#endif // WGPU_RENDERER_HPP
