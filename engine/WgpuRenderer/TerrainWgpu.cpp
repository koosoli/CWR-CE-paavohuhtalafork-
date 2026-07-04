#include "TerrainWgpu.hpp"

#include "EngineWgpu.hpp"

#include <Poseidon/World/Terrain/Landscape.hpp>

#include <algorithm>
#include <cstdint>
#include <vector>

namespace Poseidon
{
// Quads per grid-mesh axis; matches GRID_N on the Rust side so one node covers
// TerrainGridN terrain samples (roughly one quad per heightmap texel).
constexpr int TerrainGridN = 32;

TerrainWgpu::TerrainWgpu(EngineWgpu& engine, WgrRenderer* renderer) : _engine(engine), _renderer(renderer) {}

bool TerrainWgpu::UploadIfNeeded(const Landscape& land)
{
    const int range = land.GetTerrainRange();
    const char* name = land.GetName();
    if (_uploaded == &land && _uploadedRange == range && _uploadedName == name)
    {
        return false;
    }

    std::vector<float> heights(static_cast<size_t>(range) * range);
    for (int z = 0; z < range; z++)
    {
        for (int x = 0; x < range; x++)
        {
            heights[static_cast<size_t>(z) * range + x] = land.GetHeight(z, x);
        }
    }

    WgrTerrainParams params{};
    params.world_origin = {0.0f, 0.0f};
    params.land_grid = land.GetLandGrid();
    params.terrain_grid = land.GetTerrainGrid();
    params.hm_width = static_cast<uint32_t>(range);
    params.hm_height = static_cast<uint32_t>(range);
    params.land_range = static_cast<uint32_t>(land.GetLandRange());
    params.data_scale = 1.0f;
    wgr_terrain_set_heightmap(_renderer, heights.data(), &params);

    // Placeholder ground texture: a 64x64 two-tone check so UV tiling is visible.
    constexpr int groundN = 64;
    std::vector<uint8_t> px(static_cast<size_t>(groundN) * groundN * 4);
    for (int y = 0; y < groundN; y++)
    {
        for (int x = 0; x < groundN; x++)
        {
            const bool c = ((x / 8) + (y / 8)) & 1;
            uint8_t* p = &px[(static_cast<size_t>(y) * groundN + x) * 4];
            p[0] = c ? 96 : 72;
            p[1] = c ? 128 : 104;
            p[2] = c ? 72 : 56;
            p[3] = 255;
        }
    }
    wgr_terrain_set_ground_textures(_renderer, 1, groundN, groundN, WGR_TEXTURE_RGBA8, px.data(),
                                    static_cast<uint32_t>(px.size()));

    _uploaded = &land;
    _uploadedRange = range;
    _uploadedName = name;
    return true;
}

void TerrainWgpu::DrawTerrain(Scene& /*scene*/, int xBeg, int zBeg, int xEnd, int zEnd)
{
    if (_renderer == nullptr || GLandscape == nullptr)
    {
        return;
    }
    const Landscape& land = *GLandscape;
    const bool reuploaded = UploadIfNeeded(land);

    const int landRange = land.GetLandRange();
    xBeg = std::max(xBeg, 0);
    zBeg = std::max(zBeg, 0);
    xEnd = std::min(xEnd, landRange);
    zEnd = std::min(zEnd, landRange);
    if (xEnd <= xBeg || zEnd <= zBeg)
    {
        return;
    }

    // Rebuild the node tiling only when the map or visible rectangle changed.
    if (reuploaded || !_nodesValid || xBeg != _rectXBeg || zBeg != _rectZBeg || xEnd != _rectXEnd ||
        zEnd != _rectZEnd)
    {
        const float landGrid = land.GetLandGrid();
        const float nodeSize = TerrainGridN * land.GetTerrainGrid();
        const float wx1 = xEnd * landGrid;
        const float wz1 = zEnd * landGrid;

        _nodes.clear();
        for (float z = zBeg * landGrid; z < wz1; z += nodeSize)
        {
            for (float x = xBeg * landGrid; x < wx1; x += nodeSize)
            {
                WgrTerrainNode node{};
                node.origin = {x, z};
                node.size = nodeSize;
                node.lod = 0;
                _nodes.push_back(node);
            }
        }
        _rectXBeg = xBeg, _rectZBeg = zBeg, _rectXEnd = xEnd, _rectZEnd = zEnd;
        _nodesValid = true;
    }

    _engine.SubmitTerrain(_nodes);
}

} // namespace Poseidon
