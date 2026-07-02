#pragma once

#include <Poseidon/Graphics/Core/MatrixConversion.hpp>
#include <Poseidon/Graphics/Dummy/EngineDummy.hpp>
#include <Poseidon/Graphics/GraphicsEngineFactory.hpp> // GraphicsEngineParams
#include <Poseidon/Graphics/Shared/SDLEventWindow.hpp>

#include <wgpu_renderer.hpp>

#include <span>
#include <vector>

struct SDL_Window;

namespace Poseidon
{
class TextureBankWgpu;

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

    // Software-T&L path: 3D-in-UI objects (e.g. the menu laptop) arrive here with
    // CPU-projected screen-space vertices, drawn depth-tested like 2D-with-depth.
    void PrepareMesh(const render::LegacySpec& spec) override;
    void BeginMesh(TLVertexTable& mesh, const render::LegacySpec& spec) override;
    void EndMesh(TLVertexTable& mesh) override;
    void PrepareTriangle(const MipInfo& mip, int specFlags) override;
    void DrawSection(const FaceArray& face, Offset beg, Offset end) override;

    void OnWindowResized(int w, int h) override;

  private:
    // A camera-relative view/projection plus the world-space camera position the
    // per-object world matrices are offset by.
    struct CameraEntry
    {
        GfxMatrix proj;
        GfxMatrix view;
        float pos[3];
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

    SDL_Window* _window = nullptr;
    WgrRenderer* _renderer = nullptr;
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

    std::vector<CameraEntry> _cameras;
    uint32_t _currentCamera = 0;
    bool _haveCamera = false;
    GfxMatrix _world{};   // camera-relative world for the current mesh
    Matrix4 _worldM{};    // same, as an engine Matrix4 (for pre-multiplying into skin palettes)
    // Palette slot for the current skinned mesh, pre-multiplied once in BeginMeshTL
    // and shared by all its sections; WGR_NO_PALETTE when the mesh isn't skinned.
    uint32_t _currentPaletteSlot = WGR_NO_PALETTE;

    TLVertexTable* _swMesh = nullptr;
    uint64_t _swTexture = 0;
    WgrBlend _swBlend = WGR_BLEND_OPAQUE;
    Sampler2DFlags _swSampler = Sampler2DFlags::None;
    WgrDepthMode _swDepth = WGR_DEPTH_TEST_WRITE;
};

Engine* CreateEngineWgpu(const GraphicsEngineParams& params);

} // namespace Poseidon
