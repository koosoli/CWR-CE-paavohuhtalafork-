#pragma once

#include <Poseidon/Graphics/Dummy/EngineDummy.hpp>
#include <Poseidon/Graphics/GraphicsEngineFactory.hpp> // GraphicsEngineParams

#include <wgpu_renderer.h>

struct SDL_Window;

namespace Poseidon
{

// Inherits EngineDummy's no-op stubs for the full Engine surface and overrides
// only what's needed to own an SDL window and clear + present via the Rust crate.
// No real drawing yet.
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

    void HandleEvents() override;
    bool IsOpen() const override;
    void SetMouseGrab(bool grab) override;

    int Width() const override;
    int Height() const override;

    bool IsWindowed() const override;
    bool CanBeWindowed() const override;

    void NextFrame() override;

    void OnWindowResized(int w, int h) override;

  private:
    void ResizeSurface(int w, int h);

    SDL_Window* _window = nullptr;
    WgrRenderer* _renderer = nullptr;
    int _w = 0;
    int _h = 0;
    bool _windowed = true;
    bool _open = false;
    bool _focused = true;
    bool _mouseGrab = true;
};

Engine* CreateEngineWgpu(const GraphicsEngineParams& params);

} // namespace Poseidon
