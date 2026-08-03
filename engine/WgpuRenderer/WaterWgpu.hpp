#pragma once

#include <Poseidon/Graphics/Core/IWaterRenderer.hpp>
#include <Poseidon/World/Terrain/TerrainCdlod.hpp>

#include <wgpu_renderer.hpp>

#include <string>
#include <vector>

namespace Poseidon
{
class EngineWgpu;
class Landscape;

// Draws the sea via wgpu: a flat GPU CDLOD surface at the animated sea level,
// mirroring the terrain renderer. Builds a CDLOD quadtree once per map over the
// below-sea regions (pruned by terrain min-height), and each frame emits the
// selected grid nodes as GPU instances. Coastlines depth-cut the waterline; no
// per-tile shoreline geometry and no CPU regeneration on tide/wave change.
class WaterWgpu : public IWaterRenderer
{
  public:
    WaterWgpu(EngineWgpu& engine, WgrRenderer* renderer);

    void DrawWater(Scene& scene, int xBeg, int zBeg, int xEnd, int zEnd) override;

  private:
    // (Re)builds the quadtree when the map changes; returns true if it did.
    bool RebuildIfNeeded(const Landscape& land);
    void BuildQuadtree(const Landscape& land);

    EngineWgpu& _engine;
    WgrRenderer* _renderer;

    // Identity of the last build. GLandscape is a reused singleton whose data is
    // swapped in place on map switch, so the loaded terrain name is the signal that
    // a rebuild is needed (mirrors TerrainWgpu).
    const Landscape* _built = nullptr;
    int _builtRange = 0;
    std::string _builtName;

    std::vector<CdlodNode> _tree;
    int _rootIndex = -1;
    int _numLevels = 0;
    float _leafSize = 0.0f;
    std::vector<float> _ranges;

    // World-xz bounds of the (map-centred, over-sized) water tree. The per-frame
    // selection clips to these instead of the map rect, so off-map ocean out to the
    // frustum far plane is eligible; frustum + CDLOD distance + fog bound what draws.
    float _treeMin = 0.0f;
    float _treeMax = 0.0f;

    // Static placement params (world_origin/terrain_grid/hm dims), captured on build;
    // sea_level is refreshed per frame.
    WgrWaterParams _params{};
    WgrWaterInteractionParams _interaction{};
    float _lastInteractionTime = 0.0f;
    bool _haveInteractionDomain = false;
    bool _interactionDemo = false;
    int _lastInteractionDemoPulse = -1;
    bool _cameraSubmerged = false;
    // Throttling state for the submersion diagnostic in Simulate(); see the log site.
    bool _loggedCameraSubmerged = false;
    float _lastSubmersionLogTime = -1000.0f;

    // A node joins the water tree when its terrain min-height dips to or below this
    // world height (highest possible sea surface + a wave-crest margin).
    float _seaThreshold = 0.0f;

    // LOD tuning. The environment value remains an expert multiplier over the live
    // Water-tab quality preset rather than silently overriding that control.
    float _baseMult;
    float _baseMultScale;
    int _activeGeometryQuality = -1;
    float _lodRatio;
    float _morphRegion;
    // How far the ocean extends past the map, as a multiple of the map size (the tree
    // is built over `_extentFactor` x the map, centred). Frustum/fog hide the outer edge.
    float _extentFactor;

    std::vector<WgrWaterNode> _selected;
};

} // namespace Poseidon
