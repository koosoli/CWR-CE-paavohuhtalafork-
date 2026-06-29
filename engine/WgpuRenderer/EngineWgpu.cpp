#include "EngineWgpu.hpp"
#include "TextureBankWgpu.hpp"

#include <Poseidon/Core/Application.hpp>
#include <Poseidon/Foundation/Framework/AppFrame.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Graphics/Shared/SdlWindow.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Draw2DGeometry.hpp>
#include <Poseidon/Graphics/Rendering/RenderFlags.hpp>
#include <Poseidon/Graphics/Textures/TexturePreload.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/Foundation/Types/Memtype.h> // DWORD

#include <SDL3/SDL.h>

#include <cstdint>
#include <cstring>

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
    SdlGameWindowDesc wd;
    wd.title = "Poseidon [WGPU]";
    wd.width = params.width;
    wd.height = params.height;
    wd.useWindow = params.useWindow;
    wd.displayMode = params.displayMode;
    const SdlGameWindow win = CreateGameWindow(wd);
    if (!win.window)
        return;

    _window = win.window;
    _w = win.widthPx;
    _h = win.heightPx;
    _windowed = win.windowed;

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

    _eventWindow.Attach(_window, _w, _h);
}

EngineWgpu::~EngineWgpu()
{
    _eventWindow.Detach();
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

WgrBlend BlendForSpec(int spec)
{
    const render::Backend b = render::SplitLegacy(spec).backend;
    if (render::Has(b, render::Backend::IsAlpha) || render::Has(b, render::Backend::IsAlphaFog) ||
        render::Has(b, render::Backend::IsTransparent))
        return WGR_BLEND_ALPHA;
    return WGR_BLEND_OPAQUE;
}

Sampler2DFlags SamplerForSpec(int spec)
{
    const render::Backend b = render::SplitLegacy(spec).backend;
    Sampler2DFlags s = Sampler2DFlags::None;
    if (render::Has(b, render::Backend::ClampU))
        s |= Sampler2DFlags::ClampU;
    if (render::Has(b, render::Backend::ClampV))
        s |= Sampler2DFlags::ClampV;
    if (render::Has(b, render::Backend::PointSampling))
        s |= Sampler2DFlags::Point;
    return s;
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

void EngineWgpu::AppendTriangles(uint64_t texture, WgrBlend blend, Sampler2DFlags sampler, const WgrVertex2D* verts,
                                 int count)
{
    if (count <= 0)
    {
        return;
    }

    const uint32_t samplerBits = static_cast<uint32_t>(sampler);
    const uint32_t first = static_cast<uint32_t>(_verts.size());
    _verts.insert(_verts.end(), verts, verts + count);

    if (!_batches.empty() && _batches.back().texture_id == texture && _batches.back().blend == blend &&
        _batches.back().sampler == samplerBits)
    {
        _batches.back().vertex_count += static_cast<uint32_t>(count);
        return;
    }
    WgrDraw2DBatch batch{};
    batch.texture_id = texture;
    batch.first_vertex = first;
    batch.vertex_count = static_cast<uint32_t>(count);
    batch.blend = blend;
    batch.sampler = samplerBits;
    _batches.push_back(batch);
}

void EngineWgpu::Draw2D(const Draw2DPars& pars, const Rect2DAbs& rect, const Rect2DAbs& clip)
{
    if (!pars.mip.IsOK())
        return;

    Draw2DCorners c;
    if (!ClipDraw2DRect(pars, rect, clip, static_cast<float>(_w), static_cast<float>(_h), c))
    {
        return;
    }

    const WgrVertex2D tl = MakeVertex(c.xBeg, c.yBeg, c.uTL, c.vTL, pars.colorTL);
    const WgrVertex2D tr = MakeVertex(c.xEnd, c.yBeg, c.uTR, c.vTR, pars.colorTR);
    const WgrVertex2D br = MakeVertex(c.xEnd, c.yEnd, c.uBR, c.vBR, pars.colorBR);
    const WgrVertex2D bl = MakeVertex(c.xBeg, c.yEnd, c.uBL, c.vBL, pars.colorBL);
    const WgrVertex2D quad[6] = {tl, tr, br, tl, br, bl};

    AppendTriangles(ResolveTexture(pars.mip), BlendForSpec(pars.spec), SamplerForSpec(pars.spec), quad, 6);
}

void EngineWgpu::DrawPoly(const MipInfo& mip, const Vertex2DAbs* vertices, int n, const Rect2DAbs& clip, int specFlags)
{
    if (!mip.IsOK() || n < 3)
    {
        return;
    }

    constexpr int maxN = 32;
    Vertex2DAbs scratch1[maxN];
    Vertex2DAbs scratch2[maxN];
    vertices = ClipPoly2D(vertices, n, clip, scratch1, scratch2);
    if (!vertices)
    {
        return;
    }
    if (n > maxN)
    {
        n = maxN;
    }

    const uint64_t tex = ResolveTexture(mip);
    const WgrBlend blend = BlendForSpec(specFlags);
    const Sampler2DFlags sampler = SamplerForSpec(specFlags);
    std::vector<WgrVertex2D> tris;
    tris.reserve(static_cast<size_t>(n - 2) * 3);
    auto conv = [](const Vertex2DAbs& v) { return MakeVertex(v.x, v.y, v.u, v.v, v.color); };
    for (int i = 1; i + 1 < n; i++)
    {
        tris.push_back(conv(vertices[0]));
        tris.push_back(conv(vertices[i]));
        tris.push_back(conv(vertices[i + 1]));
    }
    AppendTriangles(tex, blend, sampler, tris.data(), static_cast<int>(tris.size()));
}

void EngineWgpu::DrawPoly(const MipInfo& mip, const Vertex2DPixel* vertices, int n, const Rect2DPixel& clip,
                          int specFlags)
{
    if (!mip.IsOK() || n < 3)
    {
        return;
    }

    constexpr int maxN = 32;
    Vertex2DPixel scratch1[maxN];
    Vertex2DPixel scratch2[maxN];
    vertices = ClipPoly2D(vertices, n, clip, scratch1, scratch2);
    if (!vertices)
    {
        return;
    }
    if (n > maxN)
    {
        n = maxN;
    }

    const float x2d = static_cast<float>(Left2D());
    const float y2d = static_cast<float>(Top2D());
    const uint64_t tex = ResolveTexture(mip);
    const WgrBlend blend = BlendForSpec(specFlags);
    const Sampler2DFlags sampler = SamplerForSpec(specFlags);
    std::vector<WgrVertex2D> tris;
    tris.reserve(static_cast<size_t>(n - 2) * 3);
    auto conv = [&](const Vertex2DPixel& v) { return MakeVertex(v.x + x2d, v.y + y2d, v.u, v.v, v.color); };
    for (int i = 1; i + 1 < n; i++)
    {
        tris.push_back(conv(vertices[0]));
        tris.push_back(conv(vertices[i]));
        tris.push_back(conv(vertices[i + 1]));
    }
    AppendTriangles(tex, blend, sampler, tris.data(), static_cast<int>(tris.size()));
}

void EngineWgpu::DrawLine(const Line2DAbs& line, PackedColor c0, PackedColor c1, const Rect2DAbs& clip)
{
    Texture* tex = GPreloadedTextures.New(TextureLine);
    const MipInfo& mip = TextBank()->UseMipmap(tex, 1, 1);

    const int specFlags = NoZBuf | IsAlpha | ClampU | ClampV | IsAlphaFog;

    Vertex2DAbs vertices[4];
    LineToQuad(line, c0, c1, vertices);

    DrawPoly(mip, vertices, 4, clip, specFlags);
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
