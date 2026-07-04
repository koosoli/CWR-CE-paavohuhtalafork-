#pragma once

#include <Poseidon/Graphics/Core/ITerrainRenderer.hpp>
#include <Poseidon/World/Terrain/TerrainCdlod.hpp>

#include <wgpu_renderer.hpp>

#include <string>
#include <vector>

namespace Poseidon
{
class EngineWgpu;
class Landscape;

// Draws the terrain via wgpu: uploads the heightmap once per map, builds a CDLOD
// quadtree, and each frame emits the selected grid nodes as GPU instances.
class TerrainWgpu : public ITerrainRenderer
{
  public:
    TerrainWgpu(EngineWgpu& engine, WgrRenderer* renderer);

    void DrawTerrain(Scene& scene, int xBeg, int zBeg, int xEnd, int zEnd) override;

  private:
    // (Re)uploads and rebuilds the quadtree when the map changes; returns true if it did.
    bool UploadIfNeeded(const Landscape& land);
    void BuildQuadtree(const Landscape& land);

    EngineWgpu& _engine;
    WgrRenderer* _renderer;
    // Identity of the last upload. GLandscape is a reused singleton whose data is
    // swapped in place on map switch, so the loaded terrain name is the signal
    // that a re-upload is needed.
    const Landscape* _uploaded = nullptr;
    int _uploadedRange = 0;
    std::string _uploadedName;

    std::vector<CdlodNode> _tree;
    int _rootIndex = -1;
    int _numLevels = 0;
    float _leafSize = 0.0f;
    std::vector<float> _ranges;

    // LOD tuning, read from the environment once at construction.
    float _baseMult;
    float _lodRatio;
    float _morphRegion;

    std::vector<WgrTerrainNode> _selected;
};

} // namespace Poseidon
