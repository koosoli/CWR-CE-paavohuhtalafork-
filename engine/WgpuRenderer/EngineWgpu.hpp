#pragma once

#include <Poseidon/Graphics/Core/MatrixConversion.hpp>
#include <Poseidon/Graphics/Core/TLVertex.hpp> // TLMaterial (per-draw lighting capture)
#include <Poseidon/Graphics/Dummy/EngineDummy.hpp>
#include <Poseidon/Graphics/GraphicsEngineFactory.hpp> // GraphicsEngineParams
#include <Poseidon/Graphics/Shadow/ShadowMath.hpp>
#include <Poseidon/Graphics/Shared/SDLEventWindow.hpp>

#include <wgpu_renderer.hpp>

#include <memory>
#include <span>
#include <vector>

struct SDL_Window;

namespace Poseidon
{
class TextureBankWgpu;
class TerrainWgpu;
class ITerrainRenderer;

enum class Sampler2DFlags : uint32_t
{
    None = 0,
    ClampU = 1,
    ClampV = 2,
    Point = 4,
};

constexpr Sampler2DFlags operator|(Sampler2DFlags a, Sampler2DFlags b)
{
    return static_cast<Sampler2DFlags>(static_cast<uint32_t>(a) | static_cast<uint32_t>(b));
}
constexpr Sampler2DFlags& operator|=(Sampler2DFlags& a, Sampler2DFlags b)
{
    return a = a | b;
}

// Inherits from EngineDummy, so we don't have to add manual stubs for all the missing virtual functions.
class EngineWgpu : public EngineDummy
{
  public:
    explicit EngineWgpu(const GraphicsEngineParams& params);
    ~EngineWgpu() override;

    // False if the window / wgpu device failed to come up; the factory then drops
    // this engine and falls back.
    bool IsValid() const { return _renderer != nullptr; }

    RString GetDebugName() const override;
    RString GetRendererName() const override;

    void HandleEvents() override { _eventWindow.HandleEvents(); }
    bool IsOpen() const override { return _eventWindow.IsOpen(); }
    void SetMouseGrab(bool grab) override { _eventWindow.SetMouseGrab(grab); }
    bool IsMouseGrabbed() const override { return _eventWindow.IsMouseGrabbed(); }

    int Width() const override;
    int Height() const override;

    bool IsWindowed() const override;
    bool CanBeWindowed() const override;

    AbstractTextBank* TextBank() override;

    void InitDraw(bool clear, PackedColor color) override;
    void FinishDraw() override;
    void NextFrame() override;
    void Clear(bool clearZ, bool clearColor, PackedColor color) override;

    void Draw2D(const Draw2DPars& pars, const Rect2DAbs& rect, const Rect2DAbs& clip) override;
    void DrawPoly(const MipInfo& mip, const Vertex2DAbs* vertices, int n, const Rect2DAbs& clip, int specFlags) override;
    void DrawPoly(const MipInfo& mip, const Vertex2DPixel* vertices, int n, const Rect2DPixel& clip,
                  int specFlags) override;
    void DrawLine(const Line2DAbs& line, PackedColor c0, PackedColor c1, const Rect2DAbs& clip) override;

    bool GetTL() const override { return true; }
    bool GetTLOnSurface() const override { return true; }
    bool UsesGpuSkinning() const override { return true; }
    VertexBuffer* CreateVertexBuffer(const Shape& src, VBType type) override;
    void UpdateFrameCamera() override;
    void PrepareMeshTL(const LightList& lights, const Matrix4& modelToWorld,
                       const render::LegacySpec& spec) override;
    void BeginMeshTL(const Shape& sMesh, int spec, bool dynamic) override;
    void EndMeshTL(const Shape& sMesh) override;
    void DrawSectionTL(const Shape& sMesh, int beg, int end) override;
    // Captures the per-section material so DrawSectionTL can fold it with the sun
    // (GL33 parity: emissive + sun_ambient + sun_diffuse * N.L). Called by
    // ShapeSection::PrepareTL immediately before each DrawSectionTL.
    void SetMaterial(const TLMaterial& mat, const LightList& lights, const render::LegacySpec& spec) override;

    // Software-T&L path: 3D-in-UI objects (e.g. the menu laptop) arrive here with
    // CPU-projected screen-space vertices, drawn depth-tested like 2D-with-depth.
    void PrepareMesh(const render::LegacySpec& spec) override;
    void BeginMesh(TLVertexTable& mesh, const render::LegacySpec& spec) override;
    void EndMesh(TLVertexTable& mesh) override;
    void PrepareTriangle(const MipInfo& mip, int specFlags) override;
    void DrawSection(const FaceArray& face, Offset beg, Offset end) override;

    void SetBias(int value) override { _bias = value; }
    int GetBias() override { return _bias; }
    void GetZCoefs(float& zAdd, float& zMult) override;

    // HDR tonemap/look tuning (ImGui Tonemap tab). Only meaningful with the HDR
    // resolve pass; SupportsTonemap gates the tab. SetTonemapSettings pushes to the
    // renderer immediately (takes effect next frame). In auto mode the grade is driven
    // from the per-time-of-day presets each frame (UpdateAutoTonemap in NextFrame).
    bool SupportsTonemap() const override { return _hdrEnabled && _renderer != nullptr; }
    TonemapSettings GetTonemapSettings() const override { return _tonemap; }
    void SetTonemapSettings(const TonemapSettings& s) override;
    bool GetTonemapAuto() const override { return _tonemapAuto; }
    void SetTonemapAuto(bool enable) override { _tonemapAuto = enable; }
    ExposureSettings GetExposureSettings() const override { return _exposure; }
    void SetExposureSettings(const ExposureSettings& s) override;
    float GetAutoExposureScale() const override;
    // Emit the scene->UI resolve marker (WGR_CMD_RESOLVE) into the command stream.
    void ResolveSceneToDisplay() override;

    // Procedural atmospheric sky (ImGui Sky tab). Authored params are edited here and
    // pushed to the renderer; the celestial fields are refreshed every frame from
    // LightSun in PushSky (called from NextFrame). See docs/procedural-sky-plan.md.
    bool SupportsSky() const override { return _renderer != nullptr; }
    SkySettings GetSkySettings() const override { return _sky; }
    void SetSkySettings(const SkySettings& s) override;
    // Suppress the legacy skydome on wgpu while the procedural sky is drawing.
    bool ProceduralSkyActive() const override { return _renderer != nullptr && _sky.enabled; }

    // Cascaded shadow maps, GPU-driven caster submission (SceneShadowPass).
    void SetShadowMapsEnabled(bool enabled) override { _smTuning.enabled = enabled; }
    bool ShadowMapsEnabled() const override { return _smTuning.enabled && _renderer != nullptr; }
    ShadowMapTuning GetShadowMapTuning() const override { return _smTuning; }
    void SetShadowMapTuning(const ShadowMapTuning& tuning) override
    {
        _smTuning = tuning;
        // Push the terrain sun-shadow knobs to the renderer (wgpu-only feature);
        // strength 0 = disabled. The sweep realloc/recompute happens renderer-side.
        if (_renderer)
        {
            const float strength = tuning.terrainShadowEnabled ? tuning.terrainShadowStrength : 0.0f;
            const uint32_t scale = tuning.terrainShadowScale < 1 ? 1u : uint32_t(tuning.terrainShadowScale);
            const uint32_t steps = tuning.terrainShadowSteps < 1 ? 1u : uint32_t(tuning.terrainShadowSteps);
            wgr_terrain_set_sun_shadow(_renderer, strength, scale, steps, tuning.terrainShadowPenumbra);
        }
    }
    void SetShadowMapSunFactor(float factor01) override { _smSunFactor = factor01; }
    bool UsesGpuShadowCasters() const override { return true; }
    void SetShadowCascades(const shadow::CascadeSet& cascades, int resolution) override;
    void AddShadowCaster(const Shape& mesh, const Matrix4& modelToWorld) override;
    bool DumpShadowMap(const char* path) override;
    bool ShadowDepthProbe(const float* lightVP16, const float* triXYZ, int vertCount, int res,
                          float* outDepth) override;

    bool SupportsOverlayRenderer() const override { return _renderer != nullptr; }
    uint64_t OverlayTextureCreate(int w, int h, const uint8_t* rgba) override;
    void OverlayTextureUpdate(uint64_t texture, int w, int h, const uint8_t* rgba) override;
    void OverlayTextureDestroy(uint64_t texture) override;
    void SubmitOverlay(const OverlayVertex* verts, int vertCount, const uint16_t* indices, int indexCount,
                       const OverlayDrawCmd* cmds, int cmdCount) override;

    void OnWindowResized(int w, int h) override;

    // GPU terrain renderer; null unless POSEIDON_WGPU_TERRAIN is set.
    ITerrainRenderer* GetTerrainRenderer() override;
    // Called by the terrain renderer: append a batch of `nodes` for the current
    // camera and enqueue its draw in submission order.
    void SubmitTerrain(std::span<const WgrTerrainNode> nodes);

  private:
    // A camera-relative view/projection plus the world-space camera position the
    // per-object world matrices are offset by, and the forward direction (shadow
    // cascade eye-depth select).
    struct CameraEntry
    {
        GfxMatrix proj;
        GfxMatrix view;
        float pos[3];
        float dir[3];
    };

    void ResizeSurface(int w, int h);
    // Append triangles under command `kind` (DRAW_2D or DRAW_SCREEN), merging with
    // the previous batch only when it is the most recent command of the same kind
    // and texture + blend + sampler match.
    void AppendTriangles(uint64_t texture, WgrBlend blend, Sampler2DFlags sampler, WgrDepthMode depth,
                         std::span<const WgrVertex2D> verts);
    // Push a camera entry built from the current scene camera and make it active.
    void PushSceneCamera();
    // Establish a camera for the frame's first 3D draw if none has been pushed yet.
    void EnsureCamera();

    // Pushes _tonemap to the renderer via wgr_set_tonemap (used on init + on edit).
    void PushTonemap();
    // Pushes _exposure to the renderer via wgr_set_exposure (auto-exposure params).
    void PushExposure();
    // In auto mode, interpolate the per-ToD preset for the current game time into
    // _tonemap and push it. Called once per frame from NextFrame.
    void UpdateAutoTonemap();

    // Build a WgrSky from the authored _sky params + live celestial values from
    // LightSun and push it via wgr_set_sky. Called each frame (celestial refresh)
    // and on edit from the Sky tab.
    void PushSky();

    SDL_Window* _window = nullptr;
    WgrRenderer* _renderer = nullptr;
    // HDR path enabled (mirrors the renderer's WGR_HDR gate) — gates the tonemap tab.
    bool _hdrEnabled = false;
    // Auto = drive _tonemap from the per-ToD presets; false = manual override (tab).
    bool _tonemapAuto = true;
    Engine::TonemapSettings _tonemap;
    Engine::ExposureSettings _exposure;
    // Authored procedural-sky params (atmosphere + look); celestial fields are filled
    // per frame from LightSun in PushSky.
    Engine::SkySettings _sky;
    // Smoothed celestial inputs: LightSun::Recalculate refreshes sun/moon direction,
    // night factor and fog colour only every few seconds with no interpolation, which
    // makes the sun disc + horizon haze stutter. PushSky eases these toward the live
    // values each frame (snapping on init / large jumps). See procedural-sky-plan §9.
    bool _skyInit = false;
    Vector3 _skySunDir;
    Vector3 _skyMoonDir;
    float _skyMoonPhase = 0.5f;
    float _skyNight = 0.0f;
    float _skyFog[3] = {0.7f, 0.75f, 0.8f};
    TextureBankWgpu* _wbank = nullptr;
    SDLEventWindow _eventWindow;
    int _w = 0;
    int _h = 0;
    bool _windowed = true;

    float _clear[4] = {0.0f, 0.0f, 0.0f, 1.0f};
    std::vector<WgrVertex2D> _verts;
    std::vector<WgrDraw2DBatch> _batches;
    std::vector<WgrDraw3D> _draws3d;
    std::vector<WgrCmd> _cmds;
    // Bone-matrix pool for skinned draws (128-matrix blocks; world pre-multiplied in).
    std::vector<WgrMat4> _palette;
    // Frame-global point/spot lights (rebuilt each frame in NextFrame, <= WGR_MAX_LIGHTS).
    std::vector<WgrLight> _lights;

    std::vector<CameraEntry> _cameras;
    uint32_t _currentCamera = 0;
    bool _haveCamera = false;
    GfxMatrix _world{};   // camera-relative world for the current mesh
    Matrix4 _worldM{};    // same, as an engine Matrix4 (for pre-multiplying into skin palettes)
    // Object-level spec from BeginMeshTL (IsShadow / OnSurface / z-bias / fog);
    // combined with each section's material spec in DrawSectionTL.
    int _meshSpec = 0;
    // Material captured by SetMaterial for the section about to be drawn. The
    // default (diffuse/ambient white, emissive/forcedDiffuse black) leaves an
    // unlit-by-material fallback if a draw ever reaches DrawSectionTL without a
    // preceding SetMaterial. Folded with the sun per section in DrawSectionTL.
    TLMaterial _curMaterial;
    // Current z-bias level (engine sets it via SetBias before each draw): decals
    // 0x10, ZBias overlay faces level*5, shadows 0x10/0x20. 0 = no bias.
    int _bias = 0;
    // Palette slot for the current skinned mesh, pre-multiplied once in BeginMeshTL
    // and shared by all its sections; WGR_NO_PALETTE when the mesh isn't skinned.
    uint32_t _currentPaletteSlot = WGR_NO_PALETTE;

    TLVertexTable* _swMesh = nullptr;
    uint64_t _swTexture = 0;
    WgrBlend _swBlend = WGR_BLEND_OPAQUE;
    Sampler2DFlags _swSampler = Sampler2DFlags::None;
    WgrDepthMode _swDepth = WGR_DEPTH_TEST_WRITE;

    // Cascaded-shadow state
    ShadowMapTuning _smTuning;
    float _smSunFactor = 1.0f;
    bool _smEnabledFrame = false;
    shadow::CascadeSet _smCascades;
    int _smCascadeRes = 0;
    bool _smCascadesValid = false;
    std::vector<WgrShadowCaster> _shadowCasters;
    // Camera position the shadow casters were made camera-relative to (captured in
    // AddShadowCaster). Used for the depth pass's terrain conform — must match the
    // caster world matrices, so it is NOT re-read from _currentCamera at frame end
    // (which may have advanced to a HUD/optics camera during live gameplay).
    float _smCamPos[3] = {0.0f, 0.0f, 0.0f};

    // Dev-panel overlay for the current frame
    std::vector<WgrOverlayVertex> _overlayVerts;
    std::vector<uint16_t> _overlayIndices;
    std::vector<WgrOverlayDraw> _overlayDraws;

    // GPU terrain renderer + its per-frame node/batch buffers.
    std::unique_ptr<TerrainWgpu> _terrain;
    bool _terrainEnabled = false;
    std::vector<WgrTerrainNode> _terrainNodes;
    std::vector<WgrTerrainBatch> _terrainBatches;
};

Engine* CreateEngineWgpu(const GraphicsEngineParams& params);

} // namespace Poseidon
