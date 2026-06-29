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

WGR_API const char* wgr_version(void);

/* Returns NULL on failure (reason reported via `log` if supplied). `log` may be NULL. */
WGR_API WgrRenderer* wgr_create(const WgrSurfaceDesc* desc, const WgrLogCallbacks* log);

WGR_API void wgr_destroy(WgrRenderer* renderer);
WGR_API void wgr_resize(WgrRenderer* renderer, uint32_t width, uint32_t height);

/* Returns 0 on success (incl. transient skipped frames), negative on error. */
WGR_API int32_t wgr_clear_and_present(WgrRenderer* renderer, float r, float g, float b, float a);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* WGPU_RENDERER_H */
