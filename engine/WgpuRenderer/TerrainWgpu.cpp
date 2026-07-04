#include "TerrainWgpu.hpp"

#include "EngineWgpu.hpp"
#include "TextureWgpu.hpp"

#include <Poseidon/Graphics/Textures/TextureBank.hpp>
#include <Poseidon/IO/ParamFileExt.hpp>
#include <Poseidon/World/Scene/Camera/Camera.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/World/Terrain/Landscape.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdlib>

namespace Poseidon
{
// Grid-mesh resolution per axis; must match GRID_N in terrain/mod.rs.
constexpr int TerrainGridN = 32;

static float EnvFloat(const char* name, float fallback)
{
    const char* v = std::getenv(name);
    if (v == nullptr || *v == '\0')
    {
        return fallback;
    }
    return std::strtof(v, nullptr);
}

static int BuildNode(std::vector<CdlodNode>& tree, const Landscape& land, int range, float grid, int leafTexels,
                     int oxTexel, int ozTexel, int spanTexels, int level)
{
    CdlodNode n{};
    n.originX = oxTexel * grid;
    n.originZ = ozTexel * grid;
    n.size = spanTexels * grid;
    n.level = level;
    n.child[0] = n.child[1] = n.child[2] = n.child[3] = -1;

    if (spanTexels <= leafTexels)
    {
        float mn = 1e30f;
        float mx = -1e30f;
        for (int z = ozTexel; z <= ozTexel + spanTexels; z++)
        {
            for (int x = oxTexel; x <= oxTexel + spanTexels; x++)
            {
                const float h = land.GetHeight(std::min(z, range - 1), std::min(x, range - 1));
                mn = std::min(mn, h);
                mx = std::max(mx, h);
            }
        }
        n.minY = mn;
        n.maxY = mx;
    }
    else
    {
        const int half = spanTexels / 2;
        int c[4];
        c[0] = BuildNode(tree, land, range, grid, leafTexels, oxTexel, ozTexel, half, level - 1);
        c[1] = BuildNode(tree, land, range, grid, leafTexels, oxTexel + half, ozTexel, half, level - 1);
        c[2] = BuildNode(tree, land, range, grid, leafTexels, oxTexel, ozTexel + half, half, level - 1);
        c[3] = BuildNode(tree, land, range, grid, leafTexels, oxTexel + half, ozTexel + half, half, level - 1);
        n.minY = 1e30f;
        n.maxY = -1e30f;
        for (int i = 0; i < 4; i++)
        {
            n.minY = std::min(n.minY, tree[c[i]].minY);
            n.maxY = std::max(n.maxY, tree[c[i]].maxY);
            n.child[i] = c[i];
        }
    }

    const int idx = static_cast<int>(tree.size());
    tree.push_back(n);
    return idx;
}

TerrainWgpu::TerrainWgpu(EngineWgpu& engine, WgrRenderer* renderer) : _engine(engine), _renderer(renderer)
{
    _baseMult = EnvFloat("WGR_TERRAIN_LOD_BASE", 4.0f);
    _lodRatio = EnvFloat("WGR_TERRAIN_LOD_RATIO", 2.0f);
    _morphRegion = std::clamp(EnvFloat("WGR_TERRAIN_MORPH", 0.50f), 0.05f, 1.0f);
}

void TerrainWgpu::BuildQuadtree(const Landscape& land)
{
    _tree.clear();
    _rootIndex = -1;
    _numLevels = 0;

    const int range = land.GetTerrainRange();
    const float grid = land.GetTerrainGrid();
    if (range <= 0 || grid <= 0.0f)
    {
        return;
    }

    int rootTexels = TerrainGridN;
    _numLevels = 1;
    while (rootTexels < range)
    {
        rootTexels *= 2;
        _numLevels++;
    }
    _leafSize = TerrainGridN * grid;
    _rootIndex = BuildNode(_tree, land, range, grid, TerrainGridN, 0, 0, rootTexels, _numLevels - 1);
    ComputeCdlodRanges(_leafSize * _baseMult, _lodRatio, _numLevels, _ranges);
}

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

    UploadGroundTextures(land);
    UploadIndexMap(land);
    UploadJitterMap(land);
    UploadDetailNoise();

    BuildQuadtree(land);

    _uploaded = &land;
    _uploadedRange = range;
    _uploadedName = name;
    return true;
}

void TerrainWgpu::UploadGroundTextures(const Landscape& land)
{
    // Each Landscape texture becomes one bindless layer at native size, format,
    // and mip chain, through the shared texture bank path. Missing/failed
    // textures stay handle 0 = the renderer's white fallback.
    const int layers = std::max(1, land.GetNTextures());
    std::vector<uint64_t> handles(static_cast<size_t>(layers), 0);
    for (int i = 0; i < layers; i++)
    {
        if (auto* tex = static_cast<TextureWgpu*>(land.GetTexture(i)))
        {
            handles[i] = tex->EnsureUploaded();
        }
    }
    wgr_terrain_set_ground_layers(_renderer, handles.data(), static_cast<uint32_t>(handles.size()));
}

void TerrainWgpu::UploadIndexMap(const Landscape& land)
{
    const int n = land.GetLandRange();
    if (n <= 0)
    {
        return;
    }
    // Cells must never index past the bound ground layers (the binding_array
    // carries at most WGR_TERRAIN_MAX_GROUND_LAYERS views).
    const int maxLayer = std::min(std::max(1, land.GetNTextures()),
                                  static_cast<int>(WGR_TERRAIN_MAX_GROUND_LAYERS)) - 1;
    std::vector<uint16_t> indices(static_cast<size_t>(n) * n);
    for (int z = 0; z < n; z++)
    {
        for (int x = 0; x < n; x++)
        {
            // Same (col=x, row=z) orientation as the heightmap upload;
            // GetTexture(z, x) == GetTex(x, z) is the land cell's layer index.
            // Bit 15 marks non-simple (transition) textures, which map exactly
            // once onto their cell instead of tiling — the GL33 path's
            // ClampU|ClampV (see Landscape::ClampFlags).
            const int layer = std::clamp(land.GetTexture(z, x), 0, maxLayer);
            uint16_t entry = static_cast<uint16_t>(layer);
            if (!land.TextureIsSimple(layer))
            {
                entry |= 0x8000;
            }
            indices[static_cast<size_t>(z) * n + x] = entry;
        }
    }
    wgr_terrain_set_index_map(_renderer, static_cast<uint32_t>(n), static_cast<uint32_t>(n), indices.data());
}

void TerrainWgpu::UploadJitterMap(const Landscape& land)
{
    const int n = land.GetLandRange();
    if (n <= 0)
    {
        return;
    }
    // Per-grid-point random UV offsets (Landscape::_random). GetRandomColor
    // yields at most +-0.7 UV, so the snorm quantisation loses nothing next to
    // the source's own 0.1 granularity.
    std::vector<int8_t> offsets(2 * static_cast<size_t>(n) * n);
    for (int z = 0; z < n; z++)
    {
        for (int x = 0; x < n; x++)
        {
            float u = 0.0f;
            float v = 0.0f;
            land.GetRandomColor(x, z, u, v);
            const size_t at = 2 * (static_cast<size_t>(z) * n + x);
            offsets[at + 0] = static_cast<int8_t>(std::lround(u * 127.0f));
            offsets[at + 1] = static_cast<int8_t>(std::lround(v * 127.0f));
        }
    }
    wgr_terrain_set_jitter_map(_renderer, static_cast<uint32_t>(n), static_cast<uint32_t>(n), offsets.data());
}

void TerrainWgpu::UploadDetailNoise()
{
    // Global config-driven texture; load once for the renderer's lifetime,
    // through the shared texture bank like any other texture.
    if (_detailNoiseTried)
    {
        return;
    }
    _detailNoiseTried = true;

    const ParamEntry& names = Remaster >> "CfgDetailTextures";
    RStringB detailName = names >> "detail";
    _detailNoise = GlobLoadTexture(detailName);
    Texture* tex = _detailNoise;
    if (tex == nullptr)
    {
        return;
    }
    const uint64_t handle = static_cast<TextureWgpu*>(tex)->EnsureUploaded();
    if (handle != 0)
    {
        wgr_terrain_set_detail_layer(_renderer, handle);
    }
}

void TerrainWgpu::DrawTerrain(Scene& scene, int xBeg, int zBeg, int xEnd, int zEnd)
{
    if (_renderer == nullptr || GLandscape == nullptr)
    {
        return;
    }
    const Landscape& land = *GLandscape;
    UploadIfNeeded(land);
    if (_rootIndex < 0)
    {
        return;
    }

    Camera* camera = scene.GetCamera();
    if (camera == nullptr)
    {
        return;
    }
    const Vector3 camPos = camera->Position();

    // Clip to the visible land rectangle so terrain honours the engine's draw distance.
    const float landGrid = land.GetLandGrid();
    const int landRange = land.GetLandRange();
    const float rx0 = std::max(xBeg, 0) * landGrid;
    const float rz0 = std::max(zBeg, 0) * landGrid;
    const float rx1 = std::min(xEnd, landRange) * landGrid;
    const float rz1 = std::min(zEnd, landRange) * landGrid;
    if (rx1 <= rx0 || rz1 <= rz0)
    {
        return;
    }

    auto visible = [&](const CdlodNode& n) -> bool
    {
        const float maxX = n.originX + n.size;
        const float maxZ = n.originZ + n.size;
        if (maxX <= rx0 || n.originX >= rx1 || maxZ <= rz0 || n.originZ >= rz1)
        {
            return false;
        }
        const Vector3 center(n.originX + n.size * 0.5f, (n.minY + n.maxY) * 0.5f, n.originZ + n.size * 0.5f);
        const float dy = n.maxY - n.minY;
        const float radius = 0.5f * std::sqrt(2.0f * n.size * n.size + dy * dy);
        return !camera->IsClipped(center, radius, 1);
    };
    auto emit = [&](const CdlodSelection& s)
    {
        WgrTerrainNode node{};
        node.origin = {s.originX, s.originZ};
        node.size = s.size;
        node.lod = static_cast<uint32_t>(s.level);
        node.morph_start = s.morphStart;
        node.morph_end = s.morphEnd;
        _selected.push_back(node);
    };

    _selected.clear();
    SelectCdlod(_tree, _rootIndex, _numLevels - 1, camPos.X(), camPos.Y(), camPos.Z(), _ranges, _morphRegion,
                visible, emit);

    _engine.SubmitTerrain(_selected);
}

} // namespace Poseidon
