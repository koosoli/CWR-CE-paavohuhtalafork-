#include "EngineWgpu.hpp"
#include "TerrainWgpu.hpp"
#include "TextureBankWgpu.hpp"

#include <Poseidon/Core/Application.hpp>
#include <Poseidon/Dev/Debug/DebugOverlay.hpp>
#include <Poseidon/Foundation/Framework/AppFrame.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Graphics/Core/MeshVertex.hpp>
#include <Poseidon/Graphics/Core/ZBiasMath.hpp>
#include <Poseidon/Graphics/Shared/SdlWindow.hpp>
#include <Poseidon/Graphics/Core/TLVertex.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Draw2DGeometry.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/MeshBuild.hpp>
#include <Poseidon/Graphics/Rendering/BuildRenderPassDescriptor.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Poly.hpp>
#include <Poseidon/Graphics/Rendering/Lighting/Lights.hpp>
#include <Poseidon/Graphics/Rendering/RenderFlags.hpp>
#include <Poseidon/Graphics/Rendering/Shape/ClipShape.hpp>
#include <Poseidon/Graphics/Rendering/Shape/Shape.hpp>
#include <Poseidon/Graphics/Shared/PNGWriter.hpp>
#include <Poseidon/Graphics/Textures/TexturePreload.hpp>
#include <Poseidon/World/Simulation/Animation/RtAnimation.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/World/Scene/Camera/Camera.hpp>
#include <Poseidon/Foundation/Types/Memtype.h> // DWORD

#include <SDL3/SDL.h>

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <memory>
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

// FNV-1a 64 over raw bytes; used to detect unchanged vertex data so a redundant
// GPU re-upload can be skipped. Cheap relative to the staging copy + barrier it
// avoids, and only paid on meshes whose Update trigger already fires.
inline uint64_t HashVertices(const void* data, size_t bytes)
{
    const auto* p = static_cast<const uint8_t*>(data);
    uint64_t h = 0xcbf29ce484222325ULL;
    for (size_t i = 0; i < bytes; i++)
    {
        h ^= p[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

class VertexBufferWgpu : public VertexBuffer
{
  public:
    WgrRenderer* renderer = nullptr;
    uint64_t mesh = 0;
    int vertexCount = 0;
    bool isDynamic = false;
    AutoArray<render::mesh::MeshSection> sections;

    // Content hash of the last vertex data actually uploaded, so Update can skip a
    // redundant GPU copy when the rebuilt vertices are byte-identical. VBDynamic
    // shapes (GetAllowAnimation) re-upload every frame they draw, and conformed
    // on-surface objects (roads/paths) dirty their buffer every frame in
    // Object::Animate even though their terrain-conformed geometry is static — both
    // produced a per-frame `wgr_3d_vbuf` staging copy + barrier for unchanged data.
    uint64_t lastUploadHash = 0;
    bool haveUploadHash = false;

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

    // Re-upload vertex data when the shape's geometry actually changed. `bufferDirty`
    // is the canonical mutation signal (InvalidateBuffer, raised by the CPU Animate
    // deform); `dynamic` is a caller-forced refresh; the first upload always runs.
    // Skinned meshes stay at bind pose (the GPU transforms them), so Update is a no-op.
    //
    // A clean, already-uploaded buffer is skipped in O(1) — no rebuild, no hash. This is
    // the key: `isDynamic` (VBDynamic, every GetAllowAnimation shape) used to force a full
    // rebuild + FNV content-hash EVERY frame, even when nothing changed. That was added to
    // suppress a per-instance GPU upload storm, but once terrain-conformed vegetation moved
    // to GPU conform it stopped deforming on the CPU — so its shared mesh never re-dirties,
    // yet it was still rebuilt and rehashed every frame (the dominant per-frame CPU cost in
    // profiling) to avoid a sub-millisecond upload. Trusting bufferDirty removes that waste.
    // The hash below still guards the genuinely-dirty-but-unchanged case (a CPU-deformed
    // mesh that re-derives byte-identical geometry), skipping just the GPU copy.
    void Update(const Shape& src, bool dynamic) override
    {
        if (skinned || !renderer || !mesh)
        {
            return;
        }
        // Skip when the geometry hasn't changed since it was last made current. bufferDirty
        // (InvalidateBuffer) is raised by EVERY vertex-mutating path (Object deform,
        // Animation, RtAnimation), so it is the complete change signal. `isDynamic` and the
        // `dynamic` param are only "this buffer MAY animate" hints (matSource->GetAnimated),
        // true every frame for vegetation — they must NOT force a rebuild, or conformed veg
        // (whose CPU deform is skipped, so it never re-dirties) rebuilds + hashes identical
        // vertices every frame. A static buffer is filled at creation and can't be dirtied
        // (B-028); a dynamic one is current after its first Update. Either way a clean
        // buffer needs nothing. `dynamic` is intentionally ignored (it does not mean "the
        // verts changed" — only bufferDirty does).
        if (!bufferDirty && (!isDynamic || haveUploadHash))
        {
            return;
        }
        if (src.NVertex() != vertexCount)
        {
            return;
        }
        std::vector<SVertex> verts(static_cast<size_t>(vertexCount));
        // A terrain-conform plane is active only inside a conformed object's draw
        // (ForestPlain). Then upload the UNDEFORMED mesh (OrigPos/OrigNorm) and let the
        // vertex shader conform each instance: the base bytes are identical across all
        // instances of the shape, so the content-hash below collapses what used to be a
        // per-instance upload storm to a single copy. Non-conformed draws are unchanged.
        if (GCurrentConformPlane.active && src.OriginalPosValid())
        {
            render::mesh::BuildOrigVertices(src, verts.data());
        }
        else
        {
            render::mesh::BuildVertices(src, verts.data());
        }
        bufferDirty = false;
        const uint64_t hash = HashVertices(verts.data(), verts.size() * sizeof(SVertex));
        if (haveUploadHash && hash == lastUploadHash)
        {
            return;
        }
        lastUploadHash = hash;
        haveUploadHash = true;
        wgr_mesh_update(renderer, mesh, AsMeshVerts(verts));
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
    // Neutral default material (TLMaterial's ctor only initialises specular), so a
    // draw reaching DrawSectionTL without a preceding SetMaterial lights sanely.
    _curMaterial.emmisive = HBlack;
    _curMaterial.ambient = HWhite;
    _curMaterial.diffuse = HWhite;
    _curMaterial.forcedDiffuse = HBlack;

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

    // This backend conforms terrain-clipped geometry (vegetation, roads' visuals) on
    // the GPU, so ClipLand objects publish a conform plane and skip their per-frame CPU
    // vertex deform. GL33 never sets this and keeps deforming on the CPU.
    GGpuTerrainConform = true;

    // POSEIDON_WGPU_TERRAIN=1 draws terrain via the GPU heightmap path instead of
    // the legacy per-segment Shape path.
    _terrain = std::make_unique<TerrainWgpu>(*this, _renderer);
    if (const char* t = std::getenv("POSEIDON_WGPU_TERRAIN"))
    {
        _terrainEnabled = std::strtol(t, nullptr, 10) != 0;
    }

    // WGR_SHADOW_MAPS=1 enables cascaded shadow maps at startup (dev panel /
    // tri verbs can still toggle at runtime).
    if (const char* sm = std::getenv("WGR_SHADOW_MAPS"))
    {
        _smTuning.enabled = std::strtol(sm, nullptr, 10) != 0;
    }

    Dev::DebugOverlay::InitForEngine(_window);
    _eventWindow.Attach(_window, _w, _h);
}

EngineWgpu::~EngineWgpu()
{
    // While the engine (and its renderer) is still alive so the overlay's
    // textures release cleanly; a later fallback engine can then re-init.
    Dev::DebugOverlay::Shutdown();
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

// Translate a built RenderPassDescriptor into the fields the 3D FFI carries.
// Reusing BuildRenderPassDescriptor keeps the wgpu backend's blend / depth /
// alpha-test / decal choices identical to GL33's.
WgrBlend BlendFromDesc(render::BlendMode b)
{
    switch (b)
    {
        case render::BlendMode::AlphaBlend:
            return WGR_BLEND_ALPHA;
        case render::BlendMode::Additive:
            return WGR_BLEND_ADDITIVE;
        case render::BlendMode::Shadow:
            return WGR_BLEND_SHADOW;
        case render::BlendMode::Opaque:
        default:
            return WGR_BLEND_OPAQUE;
    }
}

WgrDepthMode DepthFromDesc(render::DepthMode d)
{
    switch (d)
    {
        case render::DepthMode::Disabled:
            return WGR_DEPTH_NONE;
        case render::DepthMode::ReadOnly:
        case render::DepthMode::Shadow:
            return WGR_DEPTH_TEST;
        case render::DepthMode::Normal:
        default:
            return WGR_DEPTH_TEST_WRITE;
    }
}

// Cutout threshold in [0,1]; 0 when the draw isn't alpha-tested.
float AlphaRefFromDesc(const render::RenderPassDescriptor& desc)
{
    const bool test = desc.alpha == render::AlphaMode::Test || desc.alpha == render::AlphaMode::TestAndBlend;
    return test ? desc.alphaRef / 255.0f : 0.0f;
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
    _shadowCasters.clear();
    _smCascadesValid = false;
    _smEnabledFrame = ShadowMapsEnabled();
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
    // Engine::FinishDraw drives the frame counter (+timing) the test harness
    // readiness checks poll.
    Engine::FinishDraw();
}

void EngineWgpu::NextFrame()
{
    if (_renderer)
    {
        // Build + flatten the ImGui frame; SubmitOverlay fills the _overlay*
        // vectors the WgrFrame below points at. Composites over everything,
        // same placement as GL33's BackToFront (pre-present).
        Dev::DebugOverlay::NewFrame();
        Dev::DebugOverlay::Render();

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

        // Sun light for GPU-lit terrain, matching GL33's material upload:
        // light colour x eye accommodation, plus the raw travel direction
        // (GL33's sunDir vertex constant). At night/dawn the sun sits at or
        // below the horizon, which is what keeps terrain ambient-only dark
        // there — a made-up overhead direction reads as moonlight noon.
        Color sunDiffuse = HWhite;
        Color sunAmbient = HWhite;
        Vector3 sunDir(-0.4f, -0.85f, -0.3f);
        if (GScene && GScene->MainLight())
        {
            const Color accom = GetAccomodateEye();
            sunDiffuse = GScene->MainLight()->Diffuse() * accom;
            sunAmbient = GScene->MainLight()->Ambient() * accom;
            sunDir = GScene->MainLight()->Direction();
        }
        sunDir.Normalize();

        // Frame-global point/spot lights for the GPU-lit paths (objects + terrain).
        // Mirrors GL33's UploadVSLights, but ONE scene-wide list instead of a
        // per-draw selection: gather the scene's point/spot lights, colours
        // pre-scaled by NightEffect (so they vanish by day, like GL33's local
        // lights), positions in ABSOLUTE world space (the shader rebases per
        // camera). Clamped to the GPU buffer capacity. Per-fragment attenuation
        // discards out-of-range lights, so no distance culling is needed here yet
        // (that arrives with Forward+).
        _lights.clear();
        if (GScene && GScene->MainLight())
        {
            const float night = GScene->MainLight()->NightEffect();
            if (night > 0.0f)
            {
                const int n = GScene->NLights();
                for (int i = 0; i < n && _lights.size() < WGR_MAX_LIGHTS; i++)
                {
                    Light* light = GScene->GetLight(i);
                    if (!light || !light->IsOn())
                    {
                        continue;
                    }
                    LightDescription desc;
                    light->GetDescription(desc);
                    const bool isSpot = desc.type == LTSpotLight;
                    if (desc.type != LTPoint && !isSpot)
                    {
                        // point + spot only; the sun is the directional main light
                        continue;
                    }
                    const Color dif = desc.diffuse * night;
                    const Color amb = desc.ambient * night;
                    Vector3 beam = desc.dir;
                    beam.Normalize();
                    WgrLight pl{};
                    pl.pos = {desc.pos.X(), desc.pos.Y(), desc.pos.Z(), desc.startAtten};
                    pl.diffuse = {dif.R(), dif.G(), dif.B(), 0.0f};
                    pl.ambient = {amb.R(), amb.G(), amb.B(), 0.0f};
                    pl.dir = {beam.X(), beam.Y(), beam.Z(), isSpot ? 1.0f : 0.0f};
                    _lights.push_back(pl);
                }
            }
        }
        const float lightCount = static_cast<float>(_lights.size());

        const float shadowStrength = GetShadowFactor() / 256.0f;
        const bool shadowActive = _smCascadesValid && !_shadowCasters.empty();
        // Sun-faded darkness (GL33 parity): full daylight uses tuning.darkness,
        // dusk ramps toward 1 (no darkening) as the sun sets.
        const float darkness = 1.0f - _smSunFactor * (1.0f - _smTuning.darkness);

        std::vector<WgrCamera> cameras(_cameras.size());
        for (size_t i = 0; i < _cameras.size(); i++)
        {
            std::memcpy(cameras[i].proj.m, &_cameras[i].proj, sizeof(cameras[i].proj.m));
            std::memcpy(cameras[i].view.m, &_cameras[i].view, sizeof(cameras[i].view.m));
            cameras[i].fog_color = {fog.R(), fog.G(), fog.B(), 1.0f};
            cameras[i].params = {fogStart, fogInvRange, fogEnabled, shadowStrength};
            // cam_pos.w carries the active point-light count (the storage buffer
            // is fixed-capacity, so its length is not the count).
            cameras[i].cam_pos = {_cameras[i].pos[0], _cameras[i].pos[1], _cameras[i].pos[2], lightCount};
            cameras[i].sun_diffuse = {sunDiffuse.R(), sunDiffuse.G(), sunDiffuse.B(), 0.0f};
            cameras[i].sun_ambient = {sunAmbient.R(), sunAmbient.G(), sunAmbient.B(), 0.0f};
            cameras[i].sun_dir_world = {sunDir.X(), sunDir.Y(), sunDir.Z(), 0.0f};
            if (shadowActive)
            {
                WgrCameraShadow& sb = cameras[i].shadow;
                for (int c = 0; c < _smCascades.count && c < 4; c++)
                {
                    std::memcpy(sb.cascade_vp[c].m, _smCascades.camRelVP[c].m.data(), sizeof(sb.cascade_vp[c].m));
                }
                sb.splits = {_smCascades.splitViewDist[0], _smCascades.splitViewDist[1], _smCascades.splitViewDist[2],
                             _smCascades.splitViewDist[3]};
                sb.omni_radius = {_smCascades.omniRadius[0], _smCascades.omniRadius[1], _smCascades.omniRadius[2],
                                  _smCascades.omniRadius[3]};
                sb.ctl = {static_cast<float>(_smCascades.count), static_cast<float>(_smCascades.omniCount),
                          _smTuning.fadeRange, _smTuning.biasBase};
                sb.ctlb = {1.0f / static_cast<float>(_smCascadeRes), darkness, _smTuning.normalOffset, _smTuning.pcf};
                sb.cam_fwd = {_cameras[i].dir[0], _cameras[i].dir[1], _cameras[i].dir[2], 0.0f};
                sb.sun_dir = {_smCascades.sunDir.x, _smCascades.sunDir.y, _smCascades.sunDir.z, 0.0f};
            }
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
        if (shadowActive)
        {
            frame.shadow.count = U32(_smCascades.count);
            frame.shadow.omni_count = U32(_smCascades.omniCount);
            frame.shadow.resolution = U32(_smCascadeRes);
            for (int c = 0; c < _smCascades.count && c < 4; c++)
            {
                std::memcpy(frame.shadow.light_vp[c].m, _smCascades.camRelVP[c].m.data(),
                            sizeof(frame.shadow.light_vp[c].m));
            }
            // Casters are camera-relative to the camera captured in AddShadowCaster (NOT
            // _currentCamera, which may have advanced to a HUD/weapon camera by now); the
            // depth shader adds cam_pos back to sample the terrain conform at the right xz.
            frame.shadow.cam_pos = {_smCamPos[0], _smCamPos[1], _smCamPos[2], 0.0f};
            frame.shadow_casters = _shadowCasters;
        }
        frame.overlay_verts = _overlayVerts;
        frame.overlay_indices = _overlayIndices;
        frame.overlay_draws = _overlayDraws;
        frame.terrain_nodes = _terrainNodes;
        frame.terrain_batches = _terrainBatches;
        frame.lights = _lights;
        wgr_render_frame(_renderer, &frame);
    }
    _verts.clear();
    _batches.clear();
    _draws3d.clear();
    _cmds.clear();
    _palette.clear();
    _cameras.clear();
    _shadowCasters.clear();
    _overlayVerts.clear();
    _overlayIndices.clear();
    _overlayDraws.clear();
    _terrainNodes.clear();
    _terrainBatches.clear();
    _smCascadesValid = false;
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
        const Vector3 dir = camera->Direction();
        entry.dir[0] = dir.X();
        entry.dir[1] = dir.Y();
        entry.dir[2] = dir.Z();
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

ITerrainRenderer* EngineWgpu::GetTerrainRenderer()
{
    return (_terrainEnabled && _terrain) ? _terrain.get() : nullptr;
}

void EngineWgpu::SubmitTerrain(std::span<const WgrTerrainNode> nodes)
{
    if (!_renderer || nodes.empty())
    {
        return;
    }
    EnsureCamera();

    WgrTerrainBatch batch{};
    batch.first_node = U32(_terrainNodes.size());
    batch.node_count = U32(nodes.size());
    batch.camera = _currentCamera;
    _terrainNodes.insert(_terrainNodes.end(), nodes.begin(), nodes.end());
    _terrainBatches.push_back(batch);

    WgrCmd cmd{};
    cmd.kind = WGR_CMD_DRAW_TERRAIN;
    cmd.arg = U32(_terrainBatches.size() - 1);
    _cmds.push_back(cmd);
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

void EngineWgpu::BeginMeshTL(const Shape& sMesh, int spec, bool dynamic)
{
    _meshSpec = spec;
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

void EngineWgpu::SetMaterial(const TLMaterial& mat, const LightList& /*lights*/, const render::LegacySpec& /*spec*/)
{
    // Capture only; DrawSectionTL folds it with the sun once it has the combined
    // mesh+section spec (for the DisableSun / sun-enable decision). Per-draw local
    // lights are ignored here — the wgpu path drives lights from a single
    // frame-global light store, not GL33's per-draw list.
    _curMaterial = mat;
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

    const int sectionSpec = sMesh.GetSection(beg).properties.Special();
    const int effectiveSpec = _meshSpec | sectionSpec;
    const render::LegacySpec splitSpec = render::SplitLegacy(effectiveSpec);

    render::BuildContext ctx;
    ctx.isIn3DPass = true;
    ctx.shadowAlphaRef = static_cast<std::uint8_t>(std::min(255, (GetShadowFactor() * 7) >> 4));
    const render::RenderPassDescriptor desc = render::BuildRenderPassDescriptor(splitSpec, ctx);

    if (_smEnabledFrame && desc.blend == render::BlendMode::Shadow)
    {
        // Skip legacy shadows when CSM is enabled
        return;
    }

    WgrDraw3D d{};
    d.mesh = buf->mesh;
    d.index_begin = U32(siBeg.beg);
    d.index_count = U32(indexCount);
    d.texture_id = tex;
    std::memcpy(d.world.m, &_world, sizeof(d.world.m));
    // Terrain conform: publish this instance's bilinear plane so the vertex
    // shader conforms the (undeformed) shared mesh, matching ForestPlain::Animate. d is
    // zero-initialised, so conform2.z (mode) stays 0 for every non-conformed draw.
    if (GCurrentConformPlane.active)
    {
        const ConformPlane& cf = GCurrentConformPlane;
        if (cf.mode == 2)
        {
            // Individual ClipLand vegetation: the vertex shader samples SurfaceY per
            // vertex (mesh conform group) using bcSurfaceY; the plane fields are unused.
            d.conform0 = {cf.bcSurfaceY, 0.0f, 0.0f, 0.0f};
            d.conform1 = {0.0f, 0.0f, 0.0f, 0.0f};
            d.conform2 = {0.0f, 0.0f, 2.0f, 0.0f};
        }
        else
        {
            // ForestPlain bilinear plane (mode 1).
            d.conform0 = {cf.invLandGrid, -cf.xf, -cf.zf, cf.bias};
            d.conform1 = {cf.y00, cf.y10, cf.d1000, cf.d0100};
            d.conform2 = {cf.d1011, cf.d0111, 1.0f, 0.0f};
        }
    }
    d.blend = BlendFromDesc(desc.blend);
    d.sampler = U32(SamplerForSpec(effectiveSpec));
    d.depth = DepthFromDesc(desc.depth);
    d.alpha_ref = AlphaRefFromDesc(desc);
    // Polygon-offset selection (ignored for shadows, which get their own offset):
    //  - OnSurface routing (roads / footprint decals): the light decal offset.
    //  - Otherwise ZBias overlay faces (traffic-sign decals etc.) flagged via
    //    SetBias(level*5): a stronger, level-scaled offset. GL33 leaves these to a
    //    biased projection it never actually applies for HW-T&L, so they z-fight in
    //    wgpu; the level-scaled offset resolves them without over-biasing roads.
    d.flags = 0;
    if (desc.blend != render::BlendMode::Shadow)
    {
        if (desc.surface == render::SurfaceMode::OnSurface)
        {
            d.flags |= WGR_DRAW3D_ON_SURFACE;
        }
        else if (_bias > 0)
        {
            const uint32_t level = std::clamp(_bias / 5, 1, 3);
            d.flags |= level << WGR_DRAW3D_ZBIAS_SHIFT;
        }
    }
    d.camera = _currentCamera;
    d.palette_slot = _currentPaletteSlot;

    // Per-material sun lighting, folded exactly like GL33's
    // UploadVSMaterialConstants: raw MainLight colour x captured material, with
    // the sun-enable (!DisableSun) multiplied into the sun terms (emissive shows
    // regardless). The lit shader does emissive + sun_ambient + sun_diffuse * N.L,
    // clamped, x texture — the per-fragment analogue of GL33's VSNormal + PSNormal.
    Color sunDif = HWhite;
    Color sunAmb = HWhite;
    if (GScene && GScene->MainLight())
    {
        sunDif = GScene->MainLight()->Diffuse();
        sunAmb = GScene->MainLight()->Ambient();
    }
    const float sunEn = render::Has(splitSpec.material, render::Material::DisableSun) ? 0.0f : 1.0f;
    const Color diffuse = sunDif * _curMaterial.diffuse;
    const Color ambient = sunAmb * _curMaterial.ambient + sunDif * _curMaterial.forcedDiffuse;
    const Color emissive = _curMaterial.emmisive;
    d.mat_emissive = {emissive.R(), emissive.G(), emissive.B(), emissive.A()};
    d.mat_sun_ambient = {ambient.R() * sunEn, ambient.G() * sunEn, ambient.B() * sunEn, ambient.A() * sunEn};
    d.mat_sun_diffuse = {diffuse.R() * sunEn, diffuse.G() * sunEn, diffuse.B() * sunEn, diffuse.A() * sunEn};
    // Point/spot-light material modulation (GL33's matDif/matAmb): the raw material
    // diffuse/ambient (accommodation in, night rides the per-light colour).
    const Color& lightDif = _curMaterial.diffuse;
    const Color& lightAmb = _curMaterial.ambient;
    d.mat_light_diffuse = {lightDif.R(), lightDif.G(), lightDif.B(), lightDif.A()};
    d.mat_light_ambient = {lightAmb.R(), lightAmb.G(), lightAmb.B(), lightAmb.A()};
    // Sun-only Blinn-Phong specular, folded exactly like GL33's c18 (specCol =
    // sun->Diffuse() * mat.specular, power in w). The shader adds it per-fragment,
    // gated on w > 0, so fold the sun-enable into the colour and leave w = power.
    const Color specCol = sunDif * _curMaterial.specular;
    const float specPow = float(_curMaterial.specularPower);
    const float specEn = (specPow > 0.0f) ? sunEn : 0.0f;
    d.mat_specular = {specCol.R() * specEn, specCol.G() * specEn, specCol.B() * specEn, specPow};

    _draws3d.push_back(d);
    _cmds.push_back(WgrCmd{WGR_CMD_DRAW_3D, U32(_draws3d.size() - 1)});
}

void EngineWgpu::SetShadowCascades(const shadow::CascadeSet& cascades, int resolution)
{
    _smCascades = cascades;
    _smCascadeRes = resolution;
    _smCascadesValid = _renderer != nullptr && cascades.count > 0 && resolution > 0;
}

void EngineWgpu::AddShadowCaster(const Shape& sMesh, const Matrix4& modelToWorld)
{
    if (!_renderer || !_smCascadesValid)
    {
        return;
    }
    auto* buf = static_cast<VertexBufferWgpu*>(sMesh.GetVertexBuffer());
    if (!buf || buf->sections.Size() == 0)
    {
        return;
    }
    buf->Update(sMesh, false);

    EnsureCamera();
    const CameraEntry& cam = _cameras[_currentCamera];
    // Remember the camera these casters are made relative to, so the depth pass's terrain
    // conform reconstructs absolute world xz with the RIGHT origin (see _smCamPos).
    _smCamPos[0] = cam.pos[0];
    _smCamPos[1] = cam.pos[1];
    _smCamPos[2] = cam.pos[2];
    GfxMatrix world;
    ConvertMatrix(world, modelToWorld);
    world._41 -= cam.pos[0];
    world._42 -= cam.pos[1];
    world._43 -= cam.pos[2];

    // Skinned caster: append a palette block (mirrors BeginMeshTL) so the depth
    // pass poses it on the GPU; the scene's Animate() set buf->palette just
    // before this call.
    uint32_t paletteSlot = WGR_NO_PALETTE;
    if (buf->skinned && buf->paletteCount > 0)
    {
        buf->drawnSkinned = true;
        Matrix4 worldRel = modelToWorld;
        worldRel.SetPosition(modelToWorld.Position() - Vector3(cam.pos[0], cam.pos[1], cam.pos[2]));
        paletteSlot = U32(_palette.size() / WGR_PALETTE_SIZE);
        const size_t base = _palette.size();
        _palette.resize(base + WGR_PALETTE_SIZE);
        for (int i = 0; i < buf->paletteCount; i++)
        {
            GfxMatrix g;
            ConvertMatrix(g, worldRel * buf->palette[i]);
            std::memcpy(_palette[base + i].m, &g, sizeof(_palette[base + i].m));
        }
        GfxMatrix gw;
        ConvertMatrix(gw, worldRel);
        std::memcpy(_palette[base + kReservedBone].m, &gw, sizeof(_palette[base + kReservedBone].m));
    }

    constexpr int skipMask = NoShadow | ShadowDisabled | IsHidden | IsHiddenProxy;
    constexpr int alphaMask = IsAlpha | IsTransparent;

    // Terrain conform: SceneShadowPass publishes a mode-2 plane for individual ClipLand
    // vegetation, so the depth shader conforms this shared, undeformed mesh per vertex to
    // SurfaceY (matching the color pass and Object::Animate) instead of the CPU rewriting
    // the shadow buffer per instance. Same per-vertex mechanism as DrawSectionTL's mode 2;
    // buf->Update above uploaded OrigPos, collapsing the per-instance upload storm.
    WgrVec4 conform0{};
    WgrVec4 conform2{};
    if (GCurrentConformPlane.active && GCurrentConformPlane.mode == 2)
    {
        conform0 = {GCurrentConformPlane.bcSurfaceY, 0.0f, 0.0f, 0.0f};
        conform2 = {0.0f, 0.0f, 2.0f, 0.0f};
    }

    WgrShadowCaster run{};
    bool haveRun = false;
    auto flush = [&]
    {
        if (haveRun)
        {
            _shadowCasters.push_back(run);
            haveRun = false;
        }
    };

    for (int i = 0; i < buf->sections.Size(); i++)
    {
        const auto& props = sMesh.GetSection(i).properties;
        const shadow::CasterMode mode = shadow::ClassifyShadowCaster(props.Special(), skipMask, alphaMask);
        if (mode == shadow::CasterMode::Skip)
        {
            flush();
            continue;
        }
        uint64_t tex = 0;
        if (mode == shadow::CasterMode::AlphaTest)
        {
            if (auto* t = static_cast<TextureWgpu*>(props.GetTexture()))
            {
                tex = t->EnsureUploaded();
            }
        }
        // Cutout caster without a texture casts solid (GL33 parity).
        const float alphaRef = tex ? 0.5f : 0.0f;
        const render::mesh::MeshSection& si = buf->sections[i];
        if (haveRun && run.alpha_ref == alphaRef && run.texture_id == tex &&
            run.index_begin + run.index_count == U32(si.beg))
        {
            run.index_count += U32(si.end - si.beg);
            continue;
        }
        flush();
        run = WgrShadowCaster{};
        run.mesh = buf->mesh;
        run.index_begin = U32(si.beg);
        run.index_count = U32(si.end - si.beg);
        std::memcpy(run.world.m, &world, sizeof(run.world.m));
        run.texture_id = tex;
        run.palette_slot = paletteSlot;
        run.alpha_ref = alphaRef;
        run.sampler = 0;
        run.cascade_mask = 0xF;
        run.conform0 = conform0;
        run.conform2 = conform2;
        haveRun = true;
    }
    flush();
}

bool EngineWgpu::DumpShadowMap(const char* path)
{
    if (!_renderer || !path)
    {
        return false;
    }
    const int res = _smTuning.resolution;
    if (res <= 0)
    {
        return false;
    }
    std::vector<float> depth(static_cast<size_t>(res) * res, 1.0f);
    const uint32_t got = wgr_shadow_map_read(_renderer, 0, depth.data(), U32(depth.size()));
    if (!got)
    {
        return false;
    }
    // Same grayscale mapping as GL33's dump; wgpu rows are already top-down.
    std::vector<uint8_t> gray(static_cast<size_t>(got) * got);
    for (size_t i = 0; i < gray.size(); i++)
    {
        const float d = depth[i];
        gray[i] = (d >= 0.999f) ? static_cast<uint8_t>(35)
                                : static_cast<uint8_t>((0.15f + (1.0f - d) * 0.85f) * 255.0f);
    }
    return PNGWriter::WritePNG(path, static_cast<int>(got), static_cast<int>(got), 1, gray.data());
}

bool EngineWgpu::ShadowDepthProbe(const float* lightVP16, const float* triXYZ, int vertCount, int res, float* outDepth)
{
    if (!_renderer || !lightVP16 || !triXYZ || !outDepth || vertCount <= 0 || res <= 0)
    {
        return false;
    }
    std::vector<float> top(static_cast<size_t>(res) * res);
    if (!wgr_shadow_depth_probe(_renderer, lightVP16, triXYZ, U32(vertCount), U32(res), top.data()))
    {
        return false;
    }
    // The convention is row 0 = bottom; the wgpu readback is top-down.
    for (int y = 0; y < res; y++)
    {
        std::memcpy(outDepth + static_cast<size_t>(y) * res, top.data() + static_cast<size_t>(res - 1 - y) * res,
                    static_cast<size_t>(res) * sizeof(float));
    }
    return true;
}

uint64_t EngineWgpu::OverlayTextureCreate(int w, int h, const uint8_t* rgba)
{
    if (!_renderer || w <= 0 || h <= 0 || !rgba)
    {
        return 0;
    }
    return wgr_texture_create(_renderer, U32(w), U32(h), WGR_TEXTURE_RGBA8, 1, 0, rgba, U32(w) * U32(h) * 4);
}

void EngineWgpu::OverlayTextureUpdate(uint64_t texture, int w, int h, const uint8_t* rgba)
{
    if (!_renderer || w <= 0 || h <= 0 || !rgba)
    {
        return;
    }
    wgr_texture_update(_renderer, texture, rgba, U32(w) * U32(h) * 4);
}

void EngineWgpu::OverlayTextureDestroy(uint64_t texture)
{
    if (_renderer)
    {
        wgr_texture_destroy(_renderer, texture);
    }
}

void EngineWgpu::SubmitOverlay(const OverlayVertex* verts, int vertCount, const uint16_t* indices, int indexCount,
                               const OverlayDrawCmd* cmds, int cmdCount)
{
    static_assert(sizeof(OverlayVertex) == sizeof(WgrOverlayVertex), "overlay vertex layouts must match");
    _overlayVerts.clear();
    _overlayIndices.clear();
    _overlayDraws.clear();
    if (!verts || !indices || !cmds || vertCount <= 0 || indexCount <= 0 || cmdCount <= 0)
    {
        return;
    }
    const auto* wv = reinterpret_cast<const WgrOverlayVertex*>(verts);
    _overlayVerts.assign(wv, wv + vertCount);
    _overlayIndices.assign(indices, indices + indexCount);
    _overlayDraws.resize(cmdCount);
    for (size_t i = 0; i < _overlayDraws.size(); i++)
    {
        const OverlayDrawCmd& c = cmds[i];
        WgrOverlayDraw& d = _overlayDraws[i];
        d.clip = {c.clip[0], c.clip[1], c.clip[2], c.clip[3]};
        d.texture_id = c.texture;
        d.first_index = c.firstIndex;
        d.index_count = c.indexCount;
        d.base_vertex = c.baseVertex;
        d._pad = 0;
    }
}

void EngineWgpu::GetZCoefs(float& zAdd, float& zMult)
{
    // The software-T&L path (footsteps, tyre tracks, UI overlays) is depth-tested
    // against GPU-projected geometry; wgpu needs ~16x GL33's software z-bias to
    // clear the CPU-vs-GPU depth divergence (empirically). WGR_SW_ZBIAS_MULT tunes it.
    static const float mult = []
    {
        const char* v = std::getenv("WGR_SW_ZBIAS_MULT");
        const float m = v ? std::strtof(v, nullptr) : 16.0f;
        return m > 0.0f ? m : 16.0f;
    }();
    const auto c = render::zbias::SoftwareCoefs(_bias);
    zAdd = c.zAdd * mult;
    zMult = 1.0f - (1.0f - c.zMult) * mult;
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
