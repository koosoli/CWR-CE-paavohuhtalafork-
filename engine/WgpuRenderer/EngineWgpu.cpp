#include "EngineWgpu.hpp"
#include "TextureBankWgpu.hpp"

#include <Poseidon/Core/Application.hpp>
#include <Poseidon/Foundation/Framework/AppFrame.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Graphics/Shared/WindowMode.hpp>
#include <Poseidon/Graphics/Shared/WindowPlacement.hpp>
#include <Poseidon/Graphics/Rendering/RenderFlags.hpp>
#include <Poseidon/Graphics/Textures/TexturePreload.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
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

    _wbank = new TextureBankWgpu(_renderer);

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
    if (_wbank) {
        _wbank->Detach();
    }
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

AbstractTextBank* EngineWgpu::TextBank()
{
    return _wbank;
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
    if (w <= 0 || h <= 0)
        return;
    ResizeSurface(w, h);
    FireResizePostHook(w, h);
}

namespace
{

uint32_t BlendForSpec(int spec)
{
    const render::Backend b = render::SplitLegacy(spec).backend;
    if (render::Has(b, render::Backend::IsAlpha) || render::Has(b, render::Backend::IsAlphaFog) ||
        render::Has(b, render::Backend::IsTransparent))
        return WGR_BLEND_ALPHA;
    return WGR_BLEND_OPAQUE;
}

WgrVertex2D MakeVertex(float x, float y, float u, float v, PackedColor c)
{
    return WgrVertex2D{x, y, u, v, static_cast<uint32_t>(static_cast<DWORD>(c))};
}

uint64_t ResolveTexture(const MipInfo& mip)
{
    if (auto* t = static_cast<TextureWgpu*>(mip._texture))
    {
        return t->EnsureUploaded();
    }
    return 0;
}

} // namespace

void EngineWgpu::InitDraw(bool clear, PackedColor color)
{
    _verts.clear();
    _batches.clear();
    if (clear)
    {
        _clear[0] = color.R8() / 255.0f;
        _clear[1] = color.G8() / 255.0f;
        _clear[2] = color.B8() / 255.0f;
        _clear[3] = 1.0f;
    }
    else
    {
        _clear[0] = _clear[1] = _clear[2] = 0.0f;
        _clear[3] = 1.0f;
    }
    if (_wbank)
    {
        _wbank->StartFrame();
    }
}

void EngineWgpu::Clear(bool /*clearZ*/, bool clearColor, PackedColor color)
{
    if (clearColor)
    {
        _clear[0] = color.R8() / 255.0f;
        _clear[1] = color.G8() / 255.0f;
        _clear[2] = color.B8() / 255.0f;
        _clear[3] = 1.0f;
    }
}

void EngineWgpu::FinishDraw()
{
    if (_wbank)
    {
        _wbank->FinishFrame();
    }
}

void EngineWgpu::NextFrame()
{
    if (_renderer)
    {
        wgr_render_2d(_renderer, _clear[0], _clear[1], _clear[2], _clear[3],
                      _verts.empty() ? nullptr : _verts.data(), static_cast<uint32_t>(_verts.size()),
                      _batches.empty() ? nullptr : _batches.data(), static_cast<uint32_t>(_batches.size()));
    }
    _verts.clear();
    _batches.clear();
    EngineDummy::NextFrame();
}

void EngineWgpu::AppendTriangles(uint64_t texture, uint32_t blend, const WgrVertex2D* verts, int count)
{
    if (count <= 0)
    {
        return;
    }

    const uint32_t first = static_cast<uint32_t>(_verts.size());
    _verts.insert(_verts.end(), verts, verts + count);

    if (!_batches.empty() && _batches.back().texture_id == texture && _batches.back().blend == blend)
    {
        _batches.back().vertex_count += static_cast<uint32_t>(count);
        return;
    }
    WgrDraw2DBatch batch{};
    batch.texture_id = texture;
    batch.first_vertex = first;
    batch.vertex_count = static_cast<uint32_t>(count);
    batch.blend = blend;
    _batches.push_back(batch);
}

void EngineWgpu::Draw2D(const Draw2DPars& pars, const Rect2DAbs& rect, const Rect2DAbs& clip)
{
    if (!pars.mip.IsOK())
        return;

    // Clip the destination rect to the (window-limited) clip rect, carrying the UV range with it. 
    float xBeg = rect.x, xEnd = xBeg + rect.w;
    float yBeg = rect.y, yEnd = yBeg + rect.h;

    float uBeg = 0, vBeg = 0, uEnd = 1, vEnd = 1;

    const float xc = floatMax(clip.x, 0.0f);
    const float yc = floatMax(clip.y, 0.0f);
    const float xec = floatMin(clip.x + clip.w, static_cast<float>(_w));
    const float yec = floatMin(clip.y + clip.h, static_cast<float>(_h));

    if (xBeg < xc)
    {
        uBeg = (xc - xBeg) / rect.w, xBeg = xc;
    }
    if (xEnd > xec)
    {
        uEnd = 1 - (xEnd - xec) / rect.w, xEnd = xec;
    }
    if (yBeg < yc)
    {
        vBeg = (yc - yBeg) / rect.h, yBeg = yc;
    }
    if (yEnd > yec)
    {
        vEnd = 1 - (yEnd - yec) / rect.h, yEnd = yec;
    }

    if (xBeg >= xEnd || yBeg >= yEnd)
    {
        return;
    }

    const float uTL = pars.uTL + uBeg * (pars.uTR - pars.uTL) + vBeg * (pars.uBL - pars.uTL);
    const float uTR = pars.uTL + uEnd * (pars.uTR - pars.uTL) + vBeg * (pars.uBL - pars.uTL);
    const float uBL = pars.uTL + uBeg * (pars.uTR - pars.uTL) + vEnd * (pars.uBL - pars.uTL);
    const float uBR = pars.uTL + uEnd * (pars.uTR - pars.uTL) + vEnd * (pars.uBL - pars.uTL);

    const float vTL = pars.vTL + uBeg * (pars.vTR - pars.vTL) + vBeg * (pars.vBL - pars.vTL);
    const float vTR = pars.vTL + uEnd * (pars.vTR - pars.vTL) + vBeg * (pars.vBL - pars.vTL);
    const float vBL = pars.vTL + uBeg * (pars.vTR - pars.vTL) + vEnd * (pars.vBL - pars.vTL);
    const float vBR = pars.vTL + uEnd * (pars.vTR - pars.vTL) + vEnd * (pars.vBL - pars.vTL);

    const WgrVertex2D tl = MakeVertex(xBeg, yBeg, uTL, vTL, pars.colorTL);
    const WgrVertex2D tr = MakeVertex(xEnd, yBeg, uTR, vTR, pars.colorTR);
    const WgrVertex2D br = MakeVertex(xEnd, yEnd, uBR, vBR, pars.colorBR);
    const WgrVertex2D bl = MakeVertex(xBeg, yEnd, uBL, vBL, pars.colorBL);
    const WgrVertex2D quad[6] = {tl, tr, br, tl, br, bl};

    AppendTriangles(ResolveTexture(pars.mip), BlendForSpec(pars.spec), quad, 6);
}

void EngineWgpu::DrawPoly(const MipInfo& mip, const Vertex2DAbs* vertices, int n, const Rect2DAbs& /*clip*/,
                          int specFlags)
{
    if (!mip.IsOK() || n < 3)
    {
        return;
    }

    // TODO: Implement clipping
    const uint64_t tex = ResolveTexture(mip);
    const uint32_t blend = BlendForSpec(specFlags);
    std::vector<WgrVertex2D> tris;
    tris.reserve(static_cast<size_t>(n - 2) * 3);
    auto conv = [](const Vertex2DAbs& v) { return MakeVertex(v.x, v.y, v.u, v.v, v.color); };
    for (int i = 1; i + 1 < n; i++)
    {
        tris.push_back(conv(vertices[0]));
        tris.push_back(conv(vertices[i]));
        tris.push_back(conv(vertices[i + 1]));
    }
    AppendTriangles(tex, blend, tris.data(), static_cast<int>(tris.size()));
}

void EngineWgpu::DrawPoly(const MipInfo& mip, const Vertex2DPixel* vertices, int n, const Rect2DPixel& /*clip*/,
                          int specFlags)
{
    if (!mip.IsOK() || n < 3)
    {
        return;
    }
    // TODO: Implement clipping

    const float x2d = static_cast<float>(Left2D());
    const float y2d = static_cast<float>(Top2D());
    const uint64_t tex = ResolveTexture(mip);
    const uint32_t blend = BlendForSpec(specFlags);
    std::vector<WgrVertex2D> tris;
    tris.reserve(static_cast<size_t>(n - 2) * 3);
    auto conv = [&](const Vertex2DPixel& v) { return MakeVertex(v.x + x2d, v.y + y2d, v.u, v.v, v.color); };
    for (int i = 1; i + 1 < n; i++)
    {
        tris.push_back(conv(vertices[0]));
        tris.push_back(conv(vertices[i]));
        tris.push_back(conv(vertices[i + 1]));
    }
    AppendTriangles(tex, blend, tris.data(), static_cast<int>(tris.size()));
}

void EngineWgpu::DrawLine(const Line2DAbs& line, PackedColor c0, PackedColor c1, const Rect2DAbs& clip)
{
    // Convert the line to a textured quad
    float x0 = line.beg.x, y0 = line.beg.y, x1 = line.end.x, y1 = line.end.y;

    Texture* tex = GPreloadedTextures.New(TextureLine);
    const MipInfo& mip = TextBank()->UseMipmap(tex, 1, 1);

    const int specFlags = NoZBuf | IsAlpha | ClampU | ClampV | IsAlphaFog;
    const float dx = x1 - x0, dy = y1 - y0;
    const float dSize2 = dx * dx + dy * dy;
    const float invDSize = dSize2 > 0 ? InvSqrt(dSize2) : 1;

    const float pdx = +dy * invDSize, pdy = -dx * invDSize;
    const float w = 3.0f;
    x0 -= pdx * (w * 0.5f);
    x1 -= pdx * (w * 0.5f);
    y0 -= pdy * (w * 0.5f);
    y1 -= pdy * (w * 0.5f);
    const float x0Side = x0 + pdx * w, y0Side = y0 + pdy * w;
    const float x1Side = x1 + pdx * w, y1Side = y1 + pdy * w;

    Vertex2DAbs vertices[4];
    vertices[0].x = x0, vertices[0].y = y0, vertices[0].u = 0, vertices[0].v = 0.25f, vertices[0].color = c0;
    vertices[1].x = x0Side, vertices[1].y = y0Side, vertices[1].u = 0, vertices[1].v = 1, vertices[1].color = c0;
    vertices[2].x = x1Side, vertices[2].y = y1Side, vertices[2].u = 0.1f, vertices[2].v = 1, vertices[2].color = c1;
    vertices[3].x = x1, vertices[3].y = y1, vertices[3].u = 0.1f, vertices[3].v = 0.25f, vertices[3].color = c1;

    DrawPoly(mip, vertices, 4, clip, specFlags);
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
                {
                    GApp->m_closeRequest = true;
                }
                break;
            case SDL_EVENT_WINDOW_RESIZED:
            {
                int pw = _w, ph = _h;
                if (_window)
                {
                    SDL_GetWindowSizeInPixels(_window, &pw, &ph);
                }
                OnWindowResized(pw, ph);
                break;
            }
            case SDL_EVENT_WINDOW_FOCUS_GAINED:
                _focused = true;
                if (GApp)
                {
                    GApp->m_appActive = true;
                }
                if (_mouseGrab && _window) {
                    SDL_SetWindowRelativeMouseMode(_window, true);
                }
                break;
            case SDL_EVENT_WINDOW_FOCUS_LOST:
                _focused = false;
                if (GApp)
                {
                    GApp->m_appActive = false;
                }
                if (_window)
                {
                    SDL_SetWindowRelativeMouseMode(_window, false);
                }
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
                {
                    btn = 2;
                }
                else if (btn == 2) {
                    btn = 1;
                }
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
