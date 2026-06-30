/*
 * wgpu_renderer.h — C ABI for the WGPU graphics backend.
 */
#ifndef WGPU_RENDERER_H
#define WGPU_RENDERER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32) && !defined(WGR_STATIC)
  #define WGR_API __declspec(dllimport)
#else
  #define WGR_API
#endif

typedef struct WgrRenderer WgrRenderer;

/* Selects how WgrSurfaceDesc.window / .display are interpreted. */
typedef enum WgrPlatform
{
    WGR_PLATFORM_WIN32   = 0, /* window = HWND,         display unused        */
    WGR_PLATFORM_XLIB    = 1, /* window = Window (XID), display = Display*    */
    WGR_PLATFORM_WAYLAND = 2  /* window = wl_surface*,  display = wl_display* */
} WgrPlatform;

typedef enum WgrLogLevel
{
    WGR_LOG_TRACE = 0,
    WGR_LOG_DEBUG = 1,
    WGR_LOG_INFO  = 2,
    WGR_LOG_WARN  = 3,
    WGR_LOG_ERROR = 4
} WgrLogLevel;

typedef struct WgrSurfaceDesc
{
    WgrPlatform platform;
    void*    window;
    void*    display;
    uint32_t width;
    uint32_t height;
} WgrSurfaceDesc;

typedef struct WgrLogCallbacks
{
    /* `log` may be NULL; `msg` is only valid for the duration of the call. */
    void (*log)(int32_t level, const char* msg, void* user);
    void* user;
} WgrLogCallbacks;

/* One screen-space vertex for 2D rendering. `x`/`y` are window pixels (origin
 * top-left), `u`/`v` are texture coordinates, `color` is 0xAARRGGBB. */
typedef struct WgrVertex2D
{
    float    x, y;
    float    u, v;
    uint32_t color;
} WgrVertex2D;

typedef enum WgrBlend
{
    WGR_BLEND_OPAQUE   = 0,
    WGR_BLEND_ALPHA    = 1,
    WGR_BLEND_ADDITIVE = 2
} WgrBlend;

/* A contiguous run of triangle-list vertices sharing one texture + blend mode.
 * `texture_id` is a 64-bit generational handle from wgr_texture_create (a Rust
 * slotmap key); 0 selects the built-in 1x1 white texture. */
typedef struct WgrDraw2DBatch
{
    uint64_t texture_id;
    uint32_t first_vertex; /* index into the vertex array */
    uint32_t vertex_count; /* multiple of 3 */
    WgrBlend blend;
    uint32_t sampler;      /* sampler index: point<<2 | clampV<<1 | clampU */
} WgrDraw2DBatch;

#ifdef __cplusplus
static_assert(sizeof(WgrBlend) == 4, "WgrBlend must be 4 bytes to match the Rust #[repr(u32)] enum");
static_assert(sizeof(WgrDraw2DBatch) == 24, "WgrDraw2DBatch layout must match the Rust #[repr(C)] struct");
#endif

typedef enum WgrTexFormat
{
    WGR_TEX_RGBA8 = 0,
    WGR_TEX_BC1   = 1, /* DXT1 */
    WGR_TEX_BC2   = 2, /* DXT3 */
    WGR_TEX_BC3   = 3  /* DXT5 */
} WgrTexFormat;

WGR_API const char* wgr_version(void);

/* Returns NULL on failure (reason reported via `log` if supplied). `log` may be NULL. */
WGR_API WgrRenderer* wgr_create(const WgrSurfaceDesc* desc, const WgrLogCallbacks* log);

WGR_API void wgr_destroy(WgrRenderer* renderer);
WGR_API void wgr_resize(WgrRenderer* renderer, uint32_t width, uint32_t height);

/* Returns 0 on success (incl. transient skipped frames), negative on error. */
WGR_API int32_t wgr_clear_and_present(WgrRenderer* renderer, float r, float g, float b, float a);

/* Upload a single-level texture in `format`; returns a non-zero id, or 0 on
 * failure. `byte_len` must match the format: RGBA8 = width*height*4; BC* = the
 * block-payload size (ceil(w/4)*ceil(h/4) * 8 for BC1, * 16 for BC2/BC3). */
WGR_API uint64_t wgr_texture_create(WgrRenderer* renderer, uint32_t width, uint32_t height, int32_t format,
                                    const uint8_t* data, uint32_t byte_len);

/* Replace the pixels of an existing RGBA8 texture. */
WGR_API void wgr_texture_update(WgrRenderer* renderer, uint64_t id, const uint8_t* rgba, uint32_t byte_len);

WGR_API void wgr_texture_destroy(WgrRenderer* renderer, uint64_t id);

/* Clear to (r,g,b,a), draw all 2D batches, and present. `verts`/`batches` may be
 * null when their count is 0 (clear-only frame). Returns 0 on success. */
WGR_API int32_t wgr_render_2d(WgrRenderer* renderer, float r, float g, float b, float a, const WgrVertex2D* verts,
                              uint32_t vert_count, const WgrDraw2DBatch* batches, uint32_t batch_count);

/* One object-space mesh vertex; matches the engine's SVertex (pos, normal, uv). */
typedef struct WgrMeshVertex
{
    float px, py, pz;
    float nx, ny, nz;
    float u, v;
} WgrMeshVertex;

/* A section [index_begin, index_begin+index_count) of `mesh`, textured with
 * `texture_id` (0 = built-in white), transformed by the camera-relative `world`
 * matrix (column-major, 16 floats). `sampler` matches WgrDraw2DBatch.sampler. */
typedef struct WgrDraw3D
{
    uint64_t mesh;
    uint32_t index_begin;
    uint32_t index_count;
    uint64_t texture_id;
    float    world[16];
    WgrBlend blend;
    uint32_t sampler;
} WgrDraw3D;

#ifdef __cplusplus
static_assert(sizeof(WgrMeshVertex) == 32, "WgrMeshVertex must match the engine SVertex layout");
static_assert(sizeof(WgrDraw3D) == 96, "WgrDraw3D layout must match the Rust #[repr(C)] struct");
#endif

/* Create a static mesh from interleaved vertices + 16-bit triangle-list indices;
 * returns a non-zero handle (Rust slotmap key), or 0 on failure. */
WGR_API uint64_t wgr_mesh_create(WgrRenderer* renderer, const WgrMeshVertex* verts, uint32_t vert_count,
                                 const uint16_t* indices, uint32_t index_count);

WGR_API void wgr_mesh_destroy(WgrRenderer* renderer, uint64_t id);

/* Clear to (r,g,b,a) + depth, draw the 3D draws (depth-tested), then the 2D
 * batches on top (no depth), and present. `proj`/`view` are column-major 16-float
 * matrices. Any array may be null when its count is 0. Returns 0 on success. */
WGR_API int32_t wgr_render_frame(WgrRenderer* renderer, float r, float g, float b, float a, const float* proj,
                                 const float* view, const WgrDraw3D* draws3d, uint32_t draw_count,
                                 const WgrVertex2D* verts, uint32_t vert_count, const WgrDraw2DBatch* batches,
                                 uint32_t batch_count);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* WGPU_RENDERER_H */
