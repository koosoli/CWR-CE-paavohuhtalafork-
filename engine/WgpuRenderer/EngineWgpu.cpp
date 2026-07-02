#include "EngineWgpu.hpp"
#include "TextureBankWgpu.hpp"

#include <Poseidon/Core/Application.hpp>
#include <Poseidon/Foundation/Framework/AppFrame.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Graphics/Core/MeshVertex.hpp>
#include <Poseidon/Graphics/Shared/SdlWindow.hpp>
#include <Poseidon/Graphics/Core/TLVertex.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Draw2DGeometry.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/MeshBuild.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Poly.hpp>
#include <Poseidon/Graphics/Rendering/RenderFlags.hpp>
#include <Poseidon/Graphics/Rendering/Shape/ClipShape.hpp>
#include <Poseidon/Graphics/Rendering/Shape/Shape.hpp>
#include <Poseidon/Graphics/Textures/TexturePreload.hpp>
#include <Poseidon/World/Simulation/Animation/RtAnimation.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/World/Scene/Camera/Camera.hpp>
#include <Poseidon/Foundation/Types/Memtype.h> // DWORD

#include <SDL3/SDL.h>

#include <cstdint>
#include <cstring>
#include <span>

namespace Poseidon
{

namespace
{

// Narrowing conversion to the 32-bit width the FFI uses for counts / indices.
template <typename T>
constexpr uint32_t U32(T value)
{
    return uint32_t(value);
}

WgrSlice<WgrMeshVertex> AsMeshVerts(const std::vector<SVertex>& v)
{
    return { reinterpret_cast<const WgrMeshVertex*>(v.data()), U32(v.size()) };
}

void WgrLogThunk(int32_t level, const char* msg, void* /*user*/)
{
    if (!msg)
    {
        return;
    }
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

// Reserved palette index for unweighted vertices
constexpr int kReservedBone = WGR_PALETTE_SIZE - 1;

class VertexBufferWgpu : public VertexBuffer
{
  public:
    WgrRenderer* renderer = nullptr;
    uint64_t mesh = 0;
    int vertexCount = 0;
    bool isDynamic = false;
    AutoArray<render::mesh::MeshSection> sections;

    // GPU skinning state
    // `skinned` flips on once SetSkinData has uploaded the bind pose + weights
    // `palette` holds the current frame's model-space bone matrices
    bool skinned = false;
    int paletteCount = 0;
    std::vector<Matrix4> palette;

    VertexBufferWgpu(WgrRenderer* r, uint64_t m, int nv, bool dynamic)
        : renderer(r), mesh(m), vertexCount(nv), isDynamic(dynamic)
    {
    }
    ~VertexBufferWgpu() override
    {
        if (renderer && mesh)
        {
            wgr_mesh_destroy(renderer, mesh);
        }
    }

    // Re-upload vertex data when the shape has been animated. Mirrors GL33's
    // VertexBufferGL33::Update: refresh when the buffer is dynamic by type, the
    // caller flags this draw as dynamic, or animation dirtied the buffer. Skinned
    // meshes stay at bind pose (the GPU transforms them), so Update is a no-op.
    void Update(const Shape& src, bool dynamic) override
    {
        if (skinned || !renderer || !mesh || (!isDynamic && !dynamic && !bufferDirty))
        {
            return;
        }
        if (src.NVertex() != vertexCount)
        {
            return;
        }
        std::vector<SVertex> verts(static_cast<size_t>(vertexCount));
        render::mesh::BuildVertices(src, verts.data());
        wgr_mesh_update(renderer, mesh, AsMeshVerts(verts));
        bufferDirty = false;
    }

    // One-time: upload the bind pose (OrigPos/OrigNorm) + per-vertex bone indices
    // and weights, marking the mesh eligible for the skinned pipeline.
    void SetSkinData(const AnimationRTWeights& weightTable, const Shape& bindShape) override
    {
        if (skinned || !renderer || !mesh || bindShape.NVertex() != vertexCount)
        {
            return;
        }

        std::vector<SVertex> verts(static_cast<size_t>(vertexCount));
        std::vector<uint8_t> bones(static_cast<size_t>(vertexCount) * 4, 0);
        std::vector<uint8_t> weights(static_cast<size_t>(vertexCount) * 4, 0);
        const AnimationRTWeight* wData = weightTable.Data();
        for (int i = 0; i < vertexCount; i++)
        {
            const Vector3 p = bindShape.OrigPos(i);
            const Vector3 n = bindShape.OrigNorm(i);
            verts[i].pos = Vector3P(p.X(), p.Y(), p.Z());
            verts[i].norm = Vector3P(-n.X(), -n.Y(), -n.Z());
            verts[i].t0 = bindShape.UV(i);

            const AnimationRTWeight& w = wData[i];
            const int n4 = w.Size() < 4 ? w.Size() : 4;
            if (n4 <= 0)
            {
                // Unweighted: full weight on the reserved (world-only) bone
                bones[i * 4] = kReservedBone;
                weights[i * 4] = 255;
                continue;
            }
            for (int k = 0; k < n4; k++)
            {
                bones[i * 4 + k] = static_cast<uint8_t>(w[k].GetSel());
                // Scale weights from 0...1 to 0...255
                int q = static_cast<int>(w[k].GetWeight() * 255.0f + 0.5f);
                weights[i * 4 + k] = static_cast<uint8_t>(q < 0 ? 0 : (q > 255 ? 255 : q));
            }
        }

        wgr_mesh_update(renderer, mesh, AsMeshVerts(verts));
        wgr_mesh_set_skin(renderer, mesh, bones, weights);
        skinned = true;
    }

    void SetPalette(const Matrix4* mats, int count) override
    {
        if (count > kReservedBone)
        {
            count = kReservedBone; // never overwrite the reserved slot
        }
        paletteCount = count;
        palette.assign(mats, mats + count);
    }
};

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
    {
        return;
    }

    _window = win.window;
    _w = win.widthPx;
    _h = win.heightPx;
    _windowed = win.windowed;

    WgrSurfaceDesc desc{};
    DescribeSurface(_window, desc);
    desc.width = U32(_w > 0 ? _w : 1);
    desc.height = U32(_h > 0 ? _h : 1);

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
    if (_wbank)
    {
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
    {
        return;
    }
    _w = w;
    _h = h;
    if (_renderer)
    {
        wgr_resize(_renderer, U32(w), U32(h));
    }
}

void EngineWgpu::OnWindowResized(int w, int h)
{
    if (w <= 0 || h <= 0)
    {
        return;
    }
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
    {
        return WGR_BLEND_ALPHA;
    }
    return WGR_BLEND_OPAQUE;
}

Sampler2DFlags SamplerForSpec(int spec)
{
    const render::Backend b = render::SplitLegacy(spec).backend;
    Sampler2DFlags s = Sampler2DFlags::None;
    if (render::Has(b, render::Backend::ClampU))
    {
        s |= Sampler2DFlags::ClampU;
    }
    if (render::Has(b, render::Backend::ClampV))
    {
        s |= Sampler2DFlags::ClampV;
    }
    if (render::Has(b, render::Backend::PointSampling))
    {
        s |= Sampler2DFlags::Point;
    }
    return s;
}

WgrDepthMode DepthForSpec(int spec)
{
    const render::Backend b = render::SplitLegacy(spec).backend;
    // NoZBuf (sky) skips depth entirely; NoZWrite (transparent meshes) tests but
    // doesn't occlude; otherwise opaque geometry tests and writes.
    if (render::Has(b, render::Backend::NoZBuf))
    {
        return WGR_DEPTH_NONE;
    }
    if (render::Has(b, render::Backend::NoZWrite))
    {
        return WGR_DEPTH_TEST;
    }
    return WGR_DEPTH_TEST_WRITE;
}

// Pack an engine PackedColor into the FFI's 0xAARRGGBB WgrRgba8.
WgrRgba8 PackColor(PackedColor c)
{
    return U32(static_cast<DWORD>(c));
}

WgrVertex2D MakeVertex(float x, float y, float u, float v, PackedColor c)
{
    return WgrVertex2D {{x, y, 0.0f}, 1.0f, 1.0f, {u, v}, PackColor(c)};
}

WgrVertex2D MakeScreenVertex(const TLVertex& v)
{
    // Fog blend factor = specular alpha (GL33's vFogTC): 255 -> keep colour, 0 -> full fog.
    const float fog = v.specular.A8() / 255.0f;
    return WgrVertex2D {{v.pos[0], v.pos[1], v.pos[2]}, v.rhw, fog, {v.t0.u, v.t0.v}, PackColor(v.color)};
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
    _draws3d.clear();
    _cmds.clear();
    _palette.clear();
    _cameras.clear();
    _currentCamera = 0;
    _haveCamera = false;
    _swMesh = nullptr;
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

void EngineWgpu::Clear(bool clearZ, bool clearColor, PackedColor color)
{
    if (clearColor)
    {
        _clear[0] = color.R8() / 255.0f;
        _clear[1] = color.G8() / 255.0f;
        _clear[2] = color.B8() / 255.0f;
        _clear[3] = 1.0f;
    }
    if (clearZ)
    {
        _cmds.push_back(WgrCmd { WGR_CMD_CLEAR_DEPTH, 0 });
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
        static_assert(sizeof(CameraEntry::proj) == 64 && sizeof(CameraEntry::view) == 64,
                      "CameraEntry matrices must be 16 floats to match WgrCamera");
        static_assert(sizeof(WgrCamera::proj) == 64 && sizeof(WgrCamera::view) == 64,
                      "WgrCamera matrices must be 16 floats");
        const Color& fog = FogColor();
        // Distance fog for the 3D path, matching GL33's BuildFrameState: the
        // scene's fog range drives a linear fog factor per vertex. Packed into
        // each camera UBO (see WgrCamera). The 2D/sky path fogs separately via
        // per-vertex specular alpha.
        float fogStart = 0.0f, fogInvRange = 0.0f, fogEnabled = 0.0f;
        if (GScene)
        {
            fogStart = GScene->GetFogMinRange();
            const float fogEnd = GScene->GetFogMaxRange();
            fogInvRange = (fogEnd > fogStart) ? 1.0f / (fogEnd - fogStart) : 0.0f;
            fogEnabled = 1.0f;
        }

        std::vector<WgrCamera> cameras(_cameras.size());
        for (size_t i = 0; i < _cameras.size(); i++)
        {
            std::memcpy(cameras[i].proj.m, &_cameras[i].proj, sizeof(cameras[i].proj.m));
            std::memcpy(cameras[i].view.m, &_cameras[i].view, sizeof(cameras[i].view.m));
            cameras[i].fog_color = {fog.R(), fog.G(), fog.B(), 1.0f};
            cameras[i].fog_params = {fogStart, fogInvRange, fogEnabled, 0.0f};
        }

        WgrFrame frame{};
        frame.clear = {_clear[0], _clear[1], _clear[2], _clear[3]};
        frame.fog_color = {fog.R(), fog.G(), fog.B()};
        frame.cameras = cameras;
        frame.draws3d = _draws3d;
        frame.verts = _verts;
        frame.batches = _batches;
        frame.cmds = _cmds;
        frame.palette = _palette;
        wgr_render_frame(_renderer, &frame);
    }
    _verts.clear();
    _batches.clear();
    _draws3d.clear();
    _cmds.clear();
    _palette.clear();
    _cameras.clear();
    _haveCamera = false;
    _currentCamera = 0;
    EngineDummy::NextFrame();
}

void EngineWgpu::AppendTriangles(uint64_t texture, WgrBlend blend, Sampler2DFlags sampler, WgrDepthMode depth,
                                 std::span<const WgrVertex2D> verts)
{
    if (verts.empty())
    {
        return;
    }

    const uint32_t samplerBits = U32(sampler);
    const uint32_t first = U32(_verts.size());
    _verts.insert(_verts.end(), verts.begin(), verts.end());

    // Merge only when the previous command is this same batch; an intervening 3D
    // draw, depth clear, or any state change must break the run so submission
    // order and per-batch state are preserved.
    const bool canMerge = !_batches.empty() && !_cmds.empty() && _cmds.back().kind == WGR_CMD_DRAW_2D &&
                          _cmds.back().arg == _batches.size() - 1 && _batches.back().texture_id == texture &&
                          _batches.back().blend == blend && _batches.back().sampler == samplerBits &&
                          _batches.back().depth == depth;
    if (canMerge)
    {
        _batches.back().vertex_count += U32(verts.size());
        return;
    }
    WgrDraw2DBatch batch{};
    batch.texture_id = texture;
    batch.first_vertex = first;
    batch.vertex_count = U32(verts.size());
    batch.blend = blend;
    batch.sampler = samplerBits;
    batch.depth = depth;
    _batches.push_back(batch);
    _cmds.push_back(WgrCmd{WGR_CMD_DRAW_2D, U32(_batches.size() - 1)});
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

    AppendTriangles(ResolveTexture(pars.mip), BlendForSpec(pars.spec), SamplerForSpec(pars.spec), WGR_DEPTH_NONE, quad);
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
    AppendTriangles(tex, blend, sampler, WGR_DEPTH_NONE, tris);
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
    AppendTriangles(tex, blend, sampler, WGR_DEPTH_NONE, tris);
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

VertexBuffer* EngineWgpu::CreateVertexBuffer(const Shape& src, VBType type)
{
    if (!_renderer || src.NVertex() <= 0)
    {
        return nullptr;
    }

    const int nv = src.NVertex();
    const int ni = render::mesh::CountIndices(src);
    if (ni <= 0)
    {
        return nullptr;

    }

    static_assert(sizeof(SVertex) == sizeof(WgrMeshVertex), "SVertex must match WgrMeshVertex");
    std::vector<SVertex> verts(static_cast<size_t>(nv));
    render::mesh::BuildVertices(src, verts.data());
    std::vector<VertexIndex> indices(static_cast<size_t>(ni));
    render::mesh::BuildIndices(src, indices.data());

    const uint64_t mesh = wgr_mesh_create(
        _renderer,
        AsMeshVerts(verts),
        WgrSlice<uint16_t>{reinterpret_cast<const uint16_t*>(indices.data()), U32(ni)}
    );

    if (!mesh)
    {
        return nullptr;
    }

    const bool dynamic = (type == VBDynamic || type == VBSmallDiscardable);
    auto* buf = new VertexBufferWgpu(_renderer, mesh, nv, dynamic);
    render::mesh::BuildSections(src, buf->sections);
    return buf;
}

void EngineWgpu::PushSceneCamera()
{
    CameraEntry entry{};

    Camera* camera = GScene ? GScene->GetCamera() : nullptr;
    if (camera)
    {
        // Camera-relative: the view's translation is dropped (the per-object world
        // matrix is already offset by the camera position).  Mirrors GL33's
        // BuildFrameState so the same matrices reach the shader.
        ConvertMatrix(entry.view, camera->InverseScaled());
        entry.view._41 = 0;
        entry.view._42 = 0;
        entry.view._43 = 0;
        ConvertProjectionMatrix(entry.proj, camera->ProjectionNormal(), 0);

        const Vector3 pos = camera->Position();
        entry.pos[0] = pos.X();
        entry.pos[1] = pos.Y();
        entry.pos[2] = pos.Z();
    }

    _cameras.push_back(entry);
    _currentCamera = U32(_cameras.size() - 1);
    _haveCamera = true;
}

void EngineWgpu::EnsureCamera()
{
    if (!_haveCamera)
    {
        PushSceneCamera();
    }
}

void EngineWgpu::UpdateFrameCamera()
{
    PushSceneCamera();
}

void EngineWgpu::PrepareMeshTL(const LightList& /*lights*/, const Matrix4& modelToWorld,
                               const render::LegacySpec& /*spec*/)
{
    EnsureCamera();
    const CameraEntry& cam = _cameras[_currentCamera];

    ConvertMatrix(_world, modelToWorld);
    _world._41 -= cam.pos[0];
    _world._42 -= cam.pos[1];
    _world._43 -= cam.pos[2];

    _worldM = modelToWorld;
    _worldM.SetPosition(modelToWorld.Position() - Vector3(cam.pos[0], cam.pos[1], cam.pos[2]));
}

void EngineWgpu::BeginMeshTL(const Shape& sMesh, int /*spec*/, bool dynamic)
{
    _currentPaletteSlot = WGR_NO_PALETTE;

    auto* buf = static_cast<VertexBufferWgpu*>(sMesh.GetVertexBuffer());
    if (!buf)
    {
        return;
    }

    buf->Update(sMesh, dynamic);

    if (!buf->skinned || buf->paletteCount <= 0)
    {
        return;
    }

    // Feedback to the animation system: this mesh is GPU-skinned so its CPU skinning can be skipped next frame
    buf->drawnSkinned = true;
    _currentPaletteSlot = U32(_palette.size() / WGR_PALETTE_SIZE);
    const size_t base = _palette.size();
    _palette.resize(base + WGR_PALETTE_SIZE);
    for (int i = 0; i < buf->paletteCount; i++)
    {
        GfxMatrix g;
        ConvertMatrix(g, _worldM * buf->palette[i]);
        std::memcpy(_palette[base + i].m, &g, sizeof(_palette[base + i].m));
    }
    // Reserved slot: bare world, so unweighted verts get world*pos.
    GfxMatrix gw;
    ConvertMatrix(gw, _worldM);
    std::memcpy(_palette[base + kReservedBone].m, &gw, sizeof(_palette[base + kReservedBone].m));
}

void EngineWgpu::EndMeshTL(const Shape& /*sMesh*/)
{
    _currentPaletteSlot = WGR_NO_PALETTE;
}

void EngineWgpu::DrawSectionTL(const Shape& sMesh, int beg, int end)
{
    if (!_renderer)
    {
        return;
    }

    auto* buf = static_cast<VertexBufferWgpu*>(sMesh.GetVertexBuffer());
    if (!buf || buf->sections.Size() == 0 || end <= beg || end > buf->sections.Size())
    {
        return;
    }

    const render::mesh::MeshSection& siBeg = buf->sections[beg];
    const render::mesh::MeshSection& siEnd = buf->sections[end - 1];
    const int indexCount = siEnd.end - siBeg.beg;
    if (indexCount <= 0)
    {
        return;
    }

    uint64_t tex = 0;
    if (auto* t = static_cast<TextureWgpu*>(sMesh.GetSection(beg).properties.GetTexture()))
    {
        tex = t->EnsureUploaded();
    }

    WgrDraw3D d{};
    d.mesh = buf->mesh;
    d.index_begin = U32(siBeg.beg);
    d.index_count = U32(indexCount);
    d.texture_id = tex;
    std::memcpy(d.world.m, &_world, sizeof(d.world.m));
    d.blend = WGR_BLEND_OPAQUE;
    d.sampler = 0;
    d.camera = _currentCamera;
    d.palette_slot = _currentPaletteSlot;
    _draws3d.push_back(d);
    _cmds.push_back(WgrCmd{WGR_CMD_DRAW_3D, U32(_draws3d.size() - 1)});
}

void EngineWgpu::PrepareMesh(const render::LegacySpec& /*spec*/)
{
    _swMesh = nullptr;
}

void EngineWgpu::BeginMesh(TLVertexTable& mesh, const render::LegacySpec& /*spec*/)
{
    _swMesh = &mesh;
}

void EngineWgpu::EndMesh(TLVertexTable& /*mesh*/)
{
    _swMesh = nullptr;
}

void EngineWgpu::PrepareTriangle(const MipInfo& mip, int specFlags)
{
    _swTexture = ResolveTexture(mip);
    _swBlend = BlendForSpec(specFlags);
    _swSampler = SamplerForSpec(specFlags);
    _swDepth = DepthForSpec(specFlags);
}

void EngineWgpu::DrawSection(const FaceArray& face, Offset beg, Offset end)
{
    if (!_swMesh)
    {
        return;
    }
    const TLVertex* verts = _swMesh->VertexData();

    std::vector<WgrVertex2D> tris;
    for (Offset i = beg; i < end; face.Next(i))
    {
        const Poly& f = face[i];
        const int n = f.N();
        if (n < 3)
        {
            continue;
        }
        const VertexIndex* idx = f.GetVertexList();
        const WgrVertex2D v0 = MakeScreenVertex(verts[idx[0]]);
        for (int k = 1; k + 1 < n; k++)
        {
            tris.push_back(v0);
            tris.push_back(MakeScreenVertex(verts[idx[k]]));
            tris.push_back(MakeScreenVertex(verts[idx[k + 1]]));
        }
    }
    AppendTriangles(_swTexture, _swBlend, _swSampler, _swDepth, tris);
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
