#include "WaterWgpu.hpp"

#include "CdlodDriver.hpp"
#include "EngineWgpu.hpp"

#include <Poseidon/World/Scene/Camera/Camera.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/World/Terrain/Landscape.hpp>
#include <Poseidon/World/Terrain/LandscapeShared.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdlib>

namespace Poseidon
{
// Grid-mesh resolution per axis; must match GRID_N in water/mod.rs (and the terrain grid).
constexpr int WaterGridN = 32;

static float EnvFloat(const char* name, float fallback)
{
    const char* v = std::getenv(name);
    if (v == nullptr || *v == '\0')
    {
        return fallback;
    }
    return std::strtof(v, nullptr);
}

WaterWgpu::WaterWgpu(EngineWgpu& engine, WgrRenderer* renderer) : _engine(engine), _renderer(renderer)
{
    // Water can afford a coarser base LOD than terrain: this plan's surface is flat,
    // so near-shore tessellation buys nothing until the sibling plan adds waves.
    _baseMult = EnvFloat("WGR_WATER_LOD_BASE", 8.0f);
    _lodRatio = EnvFloat("WGR_WATER_LOD_RATIO", 2.0f);
    _morphRegion = std::clamp(EnvFloat("WGR_WATER_MORPH", 0.50f), 0.05f, 1.0f);
    // >= 1 keeps at least the map; 3 gives ~one map width of ocean past each edge,
    // which comfortably clears the far plane/fog on the stock maps.
    _extentFactor = std::max(1.0f, EnvFloat("WGR_WATER_EXTENT", 3.0f));
}

void WaterWgpu::BuildQuadtree(const Landscape& land)
{
    const int range = land.GetTerrainRange();
    const float grid = land.GetTerrainGrid();

    // Over-size the root and centre the map inside it so the ocean continues past the
    // map edges to the horizon (the legacy path fills off-map with all-water tiles).
    const int coverage = std::max(range, static_cast<int>(std::lround(_extentFactor * range)));
    const int rootTexels = CdlodRootTexels(coverage, WaterGridN);
    const int originTexel = -((rootTexels - range) / 2);

    // Same leaf min/max scan as terrain where the leaf overlaps the map (water
    // existence is defined by the terrain dipping below sea level); off-map texels are
    // open ocean. Their bound is pinned to the sea datum (0), NOT a fake deep seabed:
    // the node minY/maxY drive both the below-sea keep test AND the CDLOD frustum/LOD
    // bounding volume, and the surface is drawn at sea level regardless of any seabed —
    // a deep off-map value would sink the bounding sphere below the horizon view and
    // wrongly cull near off-map tiles. 0 <= the keep threshold, so off-map is kept.
    constexpr float OffMapSurface = 0.0f;
    auto leafBounds = [&](int ox, int oz, int span, float& mn, float& mx)
    {
        for (int z = oz; z <= oz + span; z++)
        {
            for (int x = ox; x <= ox + span; x++)
            {
                const float h =
                    (x < 0 || z < 0 || x >= range || z >= range) ? OffMapSurface : land.GetHeight(z, x);
                mn = std::min(mn, h);
                mx = std::max(mx, h);
            }
        }
    };
    BuildCdlodTree(rootTexels, originTexel, originTexel, grid, WaterGridN, leafBounds, _tree, _rootIndex,
                   _numLevels, _leafSize);
    if (_rootIndex < 0)
    {
        return;
    }
    ComputeCdlodRanges(_leafSize * _baseMult, _lodRatio, _numLevels, _ranges);
    _treeMin = originTexel * grid;
    _treeMax = (originTexel + rootTexels) * grid;

    // Highest possible sea surface (max tide) plus a wave-crest margin: nodes whose
    // terrain never rises to this height are entirely underwater and join the tree;
    // the rest are pruned. Generous + conservative — the shoreline is depth-cut, not
    // meshed, so precision here does not matter (§3 of the plan).
    _seaThreshold = maxTide + maxWave + 1.0f;

    _params = WgrWaterParams{};
    _params.world_origin = {0.0f, 0.0f};
    _params.terrain_grid = grid;
    _params.sea_level = land.GetSeaLevel();
    _params.hm_width = static_cast<uint32_t>(range);
    _params.hm_height = static_cast<uint32_t>(range);
}

bool WaterWgpu::RebuildIfNeeded(const Landscape& land)
{
    const int range = land.GetTerrainRange();
    const char* name = land.GetName();
    if (_built == &land && _builtRange == range && _builtName == name)
    {
        return false;
    }
    BuildQuadtree(land);
    _built = &land;
    _builtRange = range;
    _builtName = name;
    return true;
}

void WaterWgpu::DrawWater(Scene& scene, int xBeg, int zBeg, int xEnd, int zEnd)
{
    if (_renderer == nullptr || GLandscape == nullptr)
    {
        return;
    }
    const Landscape& land = *GLandscape;
    RebuildIfNeeded(land);
    if (_rootIndex < 0)
    {
        return;
    }

    Camera* camera = scene.GetCamera();
    if (camera == nullptr)
    {
        return;
    }

    // Refresh the animated sea level every frame; the whole plane rides at this
    // height (no mesh regeneration — the legacy path re-levelled cached vertices).
    _params.sea_level = land.GetSeaLevel();
    wgr_water_set_params(_renderer, &_params);

    // Water clips to the whole (over-sized) tree, not the engine's land draw-distance
    // rect: the ocean should reach the horizon, past where terrain stops. Frustum
    // culling + the CDLOD distance ranges + fog bound what actually draws. (xBeg..zEnd
    // stay part of the interface for the sibling look plan / future per-rect work.)
    (void)xBeg;
    (void)zBeg;
    (void)xEnd;
    (void)zEnd;
    const float rx0 = _treeMin;
    const float rz0 = _treeMin;
    const float rx1 = _treeMax;
    const float rz1 = _treeMax;

    auto emit = [&](const CdlodSelection& s)
    {
        WgrWaterNode node{};
        node.origin = {s.originX, s.originZ};
        node.size = s.size;
        node.lod = static_cast<uint32_t>(s.level);
        node.morph_start = s.morphStart;
        node.morph_end = s.morphEnd;
        _selected.push_back(node);
    };
    // Reject nodes that sit entirely above the highest sea surface: their terrain
    // never floods, so no water is drawn there (a coarse ancestor that contains any
    // below-sea descendant still passes — its aggregate minY carries the low corner).
    auto belowSea = [&](const CdlodNode& n) { return n.minY <= _seaThreshold; };

    _selected.clear();
    SelectVisibleCdlod(_tree, _rootIndex, _numLevels, _ranges, _morphRegion, *camera, rx0, rz0, rx1, rz1,
                       belowSea, emit);

    _engine.SubmitWater(_selected);
}

} // namespace Poseidon
