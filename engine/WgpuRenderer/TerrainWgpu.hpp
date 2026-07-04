#pragma once

#include <Poseidon/Graphics/Core/ITerrainRenderer.hpp>

#include <wgpu_renderer.hpp>

#include <string>
#include <vector>

namespace Poseidon
{
class EngineWgpu;
class Landscape;

// The wgpu terrain renderer. Uploads the heightmap (+ a placeholder ground
// texture) once per map and, each frame, emits grid nodes tiling the visible
// ground. Flat single-LOD for now; LOD, culling, and texture blending come later.
class TerrainWgpu : public ITerrainRenderer
{
  public:
    TerrainWgpu(EngineWgpu& engine, WgrRenderer* renderer);

    void DrawTerrain(Scene& scene, int xBeg, int zBeg, int xEnd, int zEnd) override;

  private:
    // Upload the heightmap + ground textures for `land`, skipping if unchanged.
    // Returns true if it (re)uploaded, meaning cached nodes are now stale.
    bool UploadIfNeeded(const Landscape& land);

    EngineWgpu& _engine;
    WgrRenderer* _renderer;
    // Identity of the last upload. GLandscape is a reused singleton whose data is
    // swapped in place on map switch, so the loaded terrain name is the signal
    // that a re-upload is needed.
    const Landscape* _uploaded = nullptr;
    int _uploadedRange = 0;
    std::string _uploadedName;

    // Cached node tiling; rebuilt only when the map or the visible rectangle
    // changes (the mesh itself is static from frame to frame).
    std::vector<WgrTerrainNode> _nodes;
    int _rectXBeg = 0, _rectZBeg = 0, _rectXEnd = 0, _rectZEnd = 0;
    bool _nodesValid = false;
};

} // namespace Poseidon
