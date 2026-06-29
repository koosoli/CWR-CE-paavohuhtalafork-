#include "EngineWgpu.hpp"

#include <Poseidon/Core/Application.hpp>
#include <Poseidon/Foundation/Framework/AppFrame.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Graphics/Shared/WindowMode.hpp>
#include <Poseidon/Graphics/Shared/WindowPlacement.hpp>
#include <Poseidon/Foundation/Types/Memtype.h> // DWORD

#include <SDL3/SDL.h>

#include <cstdint>
#include <cstring>

extern void SDLInput_BufferKeyEvent(SDL_Scancode sc, bool down, DWORD timestamp);
extern void SDLInput_BufferMouseButton(int btn, bool down);
extern void SDLInput_BufferMouseMotion(float dx, float dy);
extern void SDLInput_BufferMouseWheel(float dy);
extern void SDLInput_GamepadAdded(SDL_JoystickID which);
extern void SDLInput_GamepadRemoved(SDL_JoystickID which);
extern void SDLInput_BufferUIKeyEvent(SDL_Keycode key, bool down);
extern void SDLInput_BufferUICharEvent(const char* text);
extern void SetMouseAcquired(bool acquired);

namespace Poseidon
{

namespace
{

void WgrLogThunk(int32_t level, const char* msg, void* /*user*/)
{
    if (!msg)
        return;
    switch (level)
    {
        case WGR_LOG_TRACE:
        case WGR_LOG_DEBUG: LOG_DEBUG(Graphics, "wgpu: {}", msg); break;
        case WGR_LOG_WARN: LOG_WARN(Graphics, "wgpu: {}", msg); break;
        case WGR_LOG_ERROR: LOG_ERROR(Graphics, "wgpu: {}", msg); break;
        case WGR_LOG_INFO:
        default: LOG_INFO(Graphics, "wgpu: {}", msg); break;
    }
}

void DescribeSurface(SDL_Window* window, WgrSurfaceDesc& desc)
{
    const SDL_PropertiesID props = SDL_GetWindowProperties(window);
    desc.window = nullptr;
    desc.display = nullptr;
#ifdef _WIN32
    desc.platform = WGR_PLATFORM_WIN32;
    desc.window = SDL_GetPointerProperty(props, SDL_PROP_WINDOW_WIN32_HWND_POINTER, nullptr);
#else
    const char* driver = SDL_GetCurrentVideoDriver();
    if (driver && std::strcmp(driver, "wayland") == 0)
    {
        desc.platform = WGR_PLATFORM_WAYLAND;
        desc.display = SDL_GetPointerProperty(props, SDL_PROP_WINDOW_WAYLAND_DISPLAY_POINTER, nullptr);
        desc.window = SDL_GetPointerProperty(props, SDL_PROP_WINDOW_WAYLAND_SURFACE_POINTER, nullptr);
    }
    else
    {
        desc.platform = WGR_PLATFORM_XLIB;
        desc.display = SDL_GetPointerProperty(props, SDL_PROP_WINDOW_X11_DISPLAY_POINTER, nullptr);
        const Sint64 xid = SDL_GetNumberProperty(props, SDL_PROP_WINDOW_X11_WINDOW_NUMBER, 0);
        desc.window = reinterpret_cast<void*>(static_cast<uintptr_t>(xid));
    }
#endif
}

} // namespace

EngineWgpu::EngineWgpu(const GraphicsEngineParams& params) : _windowed(params.useWindow)
{
    if (!SDL_Init(SDL_INIT_VIDEO))
    {
        LOG_ERROR(Graphics, "Wgpu: SDL_Init(VIDEO) failed: {}", SDL_GetError());
        return;
    }

    int desktopW = 0, desktopH = 0, desktopRefresh = 0;
    if (const SDL_DisplayMode* dm = SDL_GetDesktopDisplayMode(SDL_GetPrimaryDisplay()))
    {
        desktopW = dm->w;
        desktopH = dm->h;
        desktopRefresh = static_cast<int>(dm->refresh_rate + 0.5f);
    }

    DisplayPlacementInput displayCfg;
    displayCfg.displayMode = params.displayMode.empty() ? (params.useWindow ? "windowed" : "borderless")
                                                        : params.displayMode;
    if (params.useWindow && displayCfg.displayMode != "windowed") {
        displayCfg.displayMode = "windowed";
    }
    if (!params.useWindow && displayCfg.displayMode == "windowed") {
        displayCfg.displayMode = "borderless";
    }
    displayCfg.width = params.width;
    displayCfg.height = params.height;

    const WindowPlacement placement = ResolveWindowPlacement(displayCfg, desktopW, desktopH, desktopRefresh);

    Uint32 flags = SDL_WINDOW_HIGH_PIXEL_DENSITY;
    switch (placement.mode)
    {
        case WindowMode::Fullscreen:
        case WindowMode::Borderless: flags |= SDL_WINDOW_BORDERLESS; break;
        case WindowMode::Windowed: flags |= SDL_WINDOW_RESIZABLE; break;
    }

    _window = SDL_CreateWindow("Poseidon [WGPU]", placement.width, placement.height, flags);
    if (!_window)
    {
        LOG_ERROR(Graphics, "Wgpu: SDL_CreateWindow failed: {}", SDL_GetError());
        return;
    }

    if (placement.mode == WindowMode::Borderless)
    {
#ifndef _WIN32
        SDL_SetWindowFullscreenMode(_window, nullptr);
        if (!SDL_SetWindowFullscreen(_window, true))
            LOG_WARN(Graphics, "Wgpu: SDL_SetWindowFullscreen(true) failed: {}", SDL_GetError());
#else
        if (placement.posX != WindowPlacement::kCentered)
            SDL_SetWindowPosition(_window, placement.posX, placement.posY);
#endif
    }
    else if (placement.posX != WindowPlacement::kCentered)
    {
        SDL_SetWindowPosition(_window, placement.posX, placement.posY);
    }

    _w = placement.width;
    _h = placement.height;
    SDL_GetWindowSizeInPixels(_window, &_w, &_h);
    _windowed = (placement.mode == WindowMode::Windowed);

    WgrSurfaceDesc desc{};
    DescribeSurface(_window, desc);
    desc.width = static_cast<uint32_t>(_w > 0 ? _w : 1);
    desc.height = static_cast<uint32_t>(_h > 0 ? _h : 1);

    const WgrLogCallbacks log{&WgrLogThunk, nullptr};
    LOG_INFO(Graphics, "Wgpu: creating renderer {} ({}x{}), crate v{}", GetRendererName().Data(), _w, _h,
             wgr_version());

    _renderer = wgr_create(&desc, &log);
    if (!_renderer)
    {
        LOG_ERROR(Graphics, "Wgpu: wgr_create failed; backend unavailable");
        SDL_DestroyWindow(_window);
        _window = nullptr;
        return;
    }

    _open = true;
    if (_mouseGrab)
        SDL_SetWindowRelativeMouseMode(_window, true);
    SDL_StartTextInput(_window);

    ::SetMouseAcquired(true);
    if (GApp)
        GApp->m_appActive = true;
}

EngineWgpu::~EngineWgpu()
{
    if (_renderer)
    {
        wgr_destroy(_renderer);
        _renderer = nullptr;
    }
    if (_window)
    {
        SDL_DestroyWindow(_window);
        _window = nullptr;
    }
}

RString EngineWgpu::GetDebugName() const
{
    return "Wgpu";
}

RString EngineWgpu::GetRendererName() const
{
    return "WGPU (Rust / wgpu)";
}

int EngineWgpu::Width() const
{
    return _w;
}

int EngineWgpu::Height() const
{
    return _h;
}

bool EngineWgpu::IsWindowed() const
{
    return _windowed;
}

bool EngineWgpu::CanBeWindowed() const
{
    return true;
}

bool EngineWgpu::IsOpen() const
{
    return _open;
}

void EngineWgpu::SetMouseGrab(bool grab)
{
    _mouseGrab = grab;
    if (_window)
        SDL_SetWindowRelativeMouseMode(_window, grab && _focused);
}

void EngineWgpu::ResizeSurface(int w, int h)
{
    if (w <= 0 || h <= 0)
        return;
    _w = w;
    _h = h;
    if (_renderer)
        wgr_resize(_renderer, static_cast<uint32_t>(w), static_cast<uint32_t>(h));
}

void EngineWgpu::OnWindowResized(int w, int h)
{
    ResizeSurface(w, h);
}

void EngineWgpu::NextFrame()
{
    if (_renderer) // nothing drawn yet — clear to a recognizable color
        wgr_clear_and_present(_renderer, 0.10f, 0.20f, 0.45f, 1.0f);
    EngineDummy::NextFrame(); // base frame timing
}

void EngineWgpu::HandleEvents()
{
    SDL_Event event;
    while (SDL_PollEvent(&event))
    {
        switch (event.type)
        {
            case SDL_EVENT_QUIT:
            case SDL_EVENT_WINDOW_CLOSE_REQUESTED:
                _open = false;
                if (GApp)
                    GApp->m_closeRequest = true;
                break;
            case SDL_EVENT_WINDOW_RESIZED:
            {
                int pw = _w, ph = _h;
                if (_window)
                    SDL_GetWindowSizeInPixels(_window, &pw, &ph);
                ResizeSurface(pw, ph);
                break;
            }
            case SDL_EVENT_WINDOW_FOCUS_GAINED:
                _focused = true;
                if (GApp)
                    GApp->m_appActive = true;
                if (_mouseGrab && _window)
                    SDL_SetWindowRelativeMouseMode(_window, true);
                break;
            case SDL_EVENT_WINDOW_FOCUS_LOST:
                _focused = false;
                if (GApp)
                    GApp->m_appActive = false;
                if (_window)
                    SDL_SetWindowRelativeMouseMode(_window, false);
                break;
            case SDL_EVENT_KEY_DOWN:
                if (!event.key.repeat)
                    SDLInput_BufferKeyEvent(event.key.scancode, true, Foundation::GlobalTickCount());
                SDLInput_BufferUIKeyEvent(event.key.key, true);
                break;
            case SDL_EVENT_KEY_UP:
                SDLInput_BufferKeyEvent(event.key.scancode, false, Foundation::GlobalTickCount());
                SDLInput_BufferUIKeyEvent(event.key.key, false);
                break;
            case SDL_EVENT_TEXT_INPUT: SDLInput_BufferUICharEvent(event.text.text); break;
            case SDL_EVENT_MOUSE_BUTTON_DOWN:
            case SDL_EVENT_MOUSE_BUTTON_UP:
            {
                int btn = event.button.button - 1;
                if (btn == 1)
                    btn = 2;
                else if (btn == 2)
                    btn = 1;
                SDLInput_BufferMouseButton(btn, event.type == SDL_EVENT_MOUSE_BUTTON_DOWN);
                break;
            }
            case SDL_EVENT_MOUSE_MOTION: SDLInput_BufferMouseMotion(event.motion.xrel, event.motion.yrel); break;
            case SDL_EVENT_MOUSE_WHEEL: SDLInput_BufferMouseWheel(event.wheel.y); break;
            case SDL_EVENT_GAMEPAD_ADDED: SDLInput_GamepadAdded(event.gdevice.which); break;
            case SDL_EVENT_GAMEPAD_REMOVED: SDLInput_GamepadRemoved(event.gdevice.which); break;
            default: break;
        }
    }
}

Engine* CreateEngineWgpu(const GraphicsEngineParams& params)
{
    auto* engine = new EngineWgpu(params);
    if (!engine->IsValid())
    {
        delete engine;
        return nullptr;
    }
    return engine;
}

} // namespace Poseidon
