#pragma once

#include <Poseidon/Graphics/Core/MatrixConversion.hpp>
#include <Poseidon/Graphics/Dummy/EngineDummy.hpp>
#include <Poseidon/Graphics/GraphicsEngineFactory.hpp> // GraphicsEngineParams
#include <Poseidon/Graphics/Shared/SDLEventWindow.hpp>

#include <wgpu_renderer.h>

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
    VertexBuffer* CreateVertexBuffer(const Shape& src, VBType type) override;
    void PrepareMeshTL(const LightList& lights, const Matrix4& modelToWorld,
                       const render::LegacySpec& spec) override;
    void BeginMeshTL(const Shape& sMesh, int spec, bool dynamic) override;
    void EndMeshTL(const Shape& sMesh) override;
    void DrawSectionTL(const Shape& sMesh, int beg, int end) override;

    void OnWindowResized(int w, int h) override;

  private:
    void ResizeSurface(int w, int h);
    // Append triangles, merging with the previous batch when texture + blend +
    // sampler match (consecutive vertices are contiguous in the buffer).
    void AppendTriangles(uint64_t texture, WgrBlend blend, Sampler2DFlags sampler, const WgrVertex2D* verts, int count);
    // Rebuild the camera-relative view/projection for this frame from the scene
    void BuildFrameCamera();

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

    GfxMatrix _proj{};
    GfxMatrix _view{};
    float _cameraPos[3] = {0.0f, 0.0f, 0.0f};
    GfxMatrix _world{};                // camera-relative world for the current mesh
    bool _frameCameraReady = false;    // view/proj rebuilt at the frame's first mesh draw
    std::vector<WgrDraw3D> _draws3d;
};

Engine* CreateEngineWgpu(const GraphicsEngineParams& params);

} // namespace Poseidon
