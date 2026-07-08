#pragma once

#include <Poseidon/Graphics/Core/ITerrainRenderer.hpp>
#include <Poseidon/Graphics/Textures/TextureBank.hpp>
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
    void UploadGroundTextures(const Landscape& land);
    void UploadIndexMap(const Landscape& land);
    void UploadJitterMap(const Landscape& land);
    // Loads the global high-frequency detail noise texture (config-driven, once).
    void UploadDetailNoise();

    EngineWgpu& _engine;
    WgrRenderer* _renderer;
    // Identity of the last upload. GLandscape is a reused singleton whose data is
    // swapped in place on map switch, so the loaded terrain name is the signal
    // that a re-upload is needed.
    const Landscape* _uploaded = nullptr;
    int _uploadedRange = 0;
    std::string _uploadedName;
    bool _detailNoiseTried = false;
    // Keeps the detail texture's GPU handle registered for the renderer's
    // lifetime (releasing the last Ref destroys the registry entry).
    Ref<Texture> _detailNoise;

    std::vector<CdlodNode> _tree;
    int _rootIndex = -1;
    int _numLevels = 0;
    float _leafSize = 0.0f;
    std::vector<float> _ranges;

    // When extended (WGR_TERRAIN_EXTENT > 1), the tree is over-sized and map-centred so
    // land continues past the map edges (clamped edge heights) to complement the
    // infinite ocean; below-sea off-map areas are pruned (the water covers them). These
    // are the tree's world-xz bounds; _extended selects the extended vs. map-only paths.
    bool _extended = false;
    float _treeMin = 0.0f;
    float _treeMax = 0.0f;

    // LOD tuning, read from the environment once at construction.
    float _baseMult;
    float _lodRatio;
    float _morphRegion;
    // Land extent past the map as a multiple of the map size (1 = map only).
    float _extentFactor;

    std::vector<WgrTerrainNode> _selected;
};

} // namespace Poseidon
