#include "TerrainWgpu.hpp"

#include "CdlodDriver.hpp"
#include "EngineWgpu.hpp"
#include "TextureWgpu.hpp"

#include <Poseidon/Core/Global.hpp> // Glob.time (coast wet-band animation clock)
#include <Poseidon/Foundation/Logging/Logging.hpp>
#include <stb_image.h>
#include <Poseidon/Graphics/Textures/TextureBank.hpp>
#include <Poseidon/IO/ParamFileExt.hpp>
#include <Poseidon/World/Scene/Camera/Camera.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/World/Terrain/Landscape.hpp>

#include <algorithm>
#include <array>
#include <cctype>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string>

namespace Poseidon
{
// Grid-mesh resolution per axis; must match GRID_N in terrain/mod.rs.
constexpr int TerrainGridN = 32;
constexpr uint32_t GrassTextureCell = 0x80000000u;

static bool ContainsNoCase(const char* text, const char* needle)
{
    if (text == nullptr || needle == nullptr || *needle == '\0')
    {
        return false;
    }
    const size_t needleLen = std::strlen(needle);
    for (const char* start = text; *start; ++start)
    {
        size_t i = 0;
        for (; i < needleLen && start[i]; ++i)
        {
            const char a = static_cast<char>(std::tolower(static_cast<unsigned char>(start[i])));
            const char b = static_cast<char>(std::tolower(static_cast<unsigned char>(needle[i])));
            if (a != b)
            {
                break;
            }
        }
        if (i == needleLen)
        {
            return true;
        }
    }
    return false;
}

static bool IsGrassTexture(const Texture* texture)
{
    if (texture == nullptr)
    {
        return false;
    }
    const char* name = texture->Name();
    // CWR/OFP terrain assets are mostly named in English or Czech.  Keep this
    // strict: an unclassified texture must not grow procedural grass over desert,
    // dirt, concrete, or rock.
    return ContainsNoCase(name, "grass") || ContainsNoCase(name, "trava") || ContainsNoCase(name, "louka") ||
           ContainsNoCase(name, "meadow") || ContainsNoCase(name, "pasture");
}

static float EnvFloat(const char* name, float fallback)
{
    const char* v = std::getenv(name);
    if (v == nullptr || *v == '\0')
    {
        return fallback;
    }
    return std::strtof(v, nullptr);
}

TerrainWgpu::TerrainWgpu(EngineWgpu& engine, WgrRenderer* renderer) : _engine(engine), _renderer(renderer)
{
    _baseMult = EnvFloat("WGR_TERRAIN_LOD_BASE", 4.0f);
    _lodRatio = EnvFloat("WGR_TERRAIN_LOD_RATIO", 2.0f);
    _morphRegion = std::clamp(EnvFloat("WGR_TERRAIN_MORPH", 0.50f), 0.05f, 1.0f);
    // >1 extends terrain past the map edges (clamped edge heights) to complement the
    // infinite ocean; 1 = map only. Defaults to match the water extent (WGR_WATER_EXTENT)
    // so seabed underlies the whole transparent ocean with no seam at the old map edge.
    _extentFactor = std::max(1.0f, EnvFloat("WGR_TERRAIN_EXTENT", 3.0f));
}

void TerrainWgpu::BuildQuadtree(const Landscape& land)
{
    const int range = land.GetTerrainRange();
    const float grid = land.GetTerrainGrid();

    // Optionally over-size + centre the root so land continues past the map edges. At
    // extent 1 this is exactly the map-only root at origin 0 (byte-identical selection).
    _extended = _extentFactor > 1.0f;
    const int coverage = _extended ? std::max(range, static_cast<int>(std::lround(_extentFactor * range))) : range;
    const int rootTexels = CdlodRootTexels(coverage, TerrainGridN);
    const int originTexel = _extended ? CdlodCenteredOrigin(rootTexels, range, TerrainGridN) : 0;

    // Each leaf's world-height extent is scanned from the heightmap (same per-texel
    // min/max the CDLOD selection frustum-tests against). Off-map texels clamp to the
    // nearest edge height — the vertex shader samples the heightmap the same clamped
    // way, so the extended border is a flat continuation of the boundary terrain.
    auto leafBounds = [&](int ox, int oz, int span, float& mn, float& mx)
    {
        for (int z = oz; z <= oz + span; z++)
        {
            for (int x = ox; x <= ox + span; x++)
            {
                const float h = land.GetHeight(std::clamp(z, 0, range - 1), std::clamp(x, 0, range - 1));
                mn = std::min(mn, h);
                mx = std::max(mx, h);
            }
        }
    };
    BuildCdlodTree(rootTexels, originTexel, originTexel, grid, TerrainGridN, leafBounds, _tree, _rootIndex, _numLevels,
                   _leafSize);
    if (_rootIndex < 0)
    {
        return;
    }
    ComputeCdlodRanges(_leafSize * _baseMult, _lodRatio, _numLevels, _ranges);
    _treeMin = originTexel * grid;
    _treeMax = (originTexel + rootTexels) * grid;
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

    // Fill the static params (the coast wet-band fields are refreshed per frame in DrawTerrain).
    _params.world_origin = {0.0f, 0.0f};
    _params.land_grid = land.GetLandGrid();
    _params.terrain_grid = land.GetTerrainGrid();
    _params.hm_width = static_cast<uint32_t>(range);
    _params.hm_height = static_cast<uint32_t>(range);
    _params.land_range = static_cast<uint32_t>(land.GetLandRange());
    _params.data_scale = 1.0f;
    wgr_terrain_set_heightmap(_renderer, heights.data(), &_params);

    UploadGroundTextures(land);
    UploadIndexMap(land);
    UploadGeography(land);
    UploadJitterMap(land);
    UploadDetailNoise();

    BuildQuadtree(land);

    _uploaded = &land;
    _uploadedRange = range;
    _uploadedName = name;
    // Confirmed from the installed CWA world data: Eden is Everon. Its raw
    // GeographyInfo flags are not reliable grass exclusions, unlike the other
    // stock worlds, so the shader must rely on exact height/slope plus the
    // explicit surface controls for this map.
    _grassNeedsCompatibilityOverride = ContainsNoCase(name, "eden.wrp");
    if (_grassNeedsCompatibilityOverride)
    {
        LOG_WARN(Graphics, "Wgpu grass: enabling Everon/Eden legacy geography compatibility override");
    }
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
    const int maxLayer =
        std::min(std::max(1, land.GetNTextures()), static_cast<int>(WGR_TERRAIN_MAX_GROUND_LAYERS)) - 1;
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

void TerrainWgpu::UploadGeography(const Landscape& land)
{
    const int n = land.GetLandRange();
    if (n <= 0)
    {
        return;
    }
    const int layerCount = std::max(0, land.GetNTextures());
    bool layersChanged = static_cast<int>(_grassSurfaceNames.size()) != layerCount;
    if (!layersChanged)
    {
        for (int i = 0; i < layerCount; ++i)
        {
            const char* name = land.GetTexture(i) ? land.GetTexture(i)->Name() : "<null>";
            if (_grassSurfaceNames[static_cast<size_t>(i)] != name)
            {
                layersChanged = true;
                break;
            }
        }
    }
    if (layersChanged)
    {
        _grassSurfaceNames.clear();
        _grassSurfaceEnabled.clear();
        _grassSurfaceNames.reserve(static_cast<size_t>(layerCount));
        _grassSurfaceEnabled.reserve(static_cast<size_t>(layerCount));
        for (int i = 0; i < layerCount; ++i)
        {
            const Texture* texture = land.GetTexture(i);
            _grassSurfaceNames.emplace_back(texture ? texture->Name() : "<null>");
            _grassSurfaceEnabled.push_back(IsGrassTexture(texture));
        }

        // The original CWA islands (including Everon/Eden) store their
        // terrain materials under two-character codes such as eden\\pl and
        // eden\\tn. They contain no semantic "grass" word, so the strict
        // detector above selected *zero* surfaces and made grass impossible
        // to load. Fall back to every terrain layer only when nothing could
        // be named; water, roads, forests, buildings, cliffs, and shore are
        // still rejected by the authoritative geography/slope checks in the
        // grass shader. The Grass tab remains able to narrow this selection.
        const bool noNamedGrass = std::none_of(_grassSurfaceEnabled.begin(), _grassSurfaceEnabled.end(),
                                               [](bool enabled) { return enabled; });
        if (noNamedGrass && layerCount > 0)
        {
            std::fill(_grassSurfaceEnabled.begin(), _grassSurfaceEnabled.end(), true);
            LOG_INFO(Graphics,
                     "Wgpu grass: no named grass textures on this legacy world; enabling all {} terrain layers",
                     layerCount);
        }
    }

    std::vector<bool> grassLayer(static_cast<size_t>(layerCount), false);
    std::string grassLayerNames;
    std::string allLayerNames;
    int grassLayerCount = 0;
    for (int i = 0; i < land.GetNTextures(); ++i)
    {
        if (!allLayerNames.empty())
        {
            allLayerNames += ", ";
        }
        allLayerNames += std::to_string(i);
        allLayerNames += ":";
        allLayerNames += land.GetTexture(i) ? land.GetTexture(i)->Name() : "<null>";
        grassLayer[static_cast<size_t>(i)] = _grassSurfaceEnabled[static_cast<size_t>(i)];
        if (grassLayer[static_cast<size_t>(i)])
        {
            if (!grassLayerNames.empty())
            {
                grassLayerNames += ", ";
            }
            grassLayerNames += land.GetTexture(i)->Name();
            ++grassLayerCount;
        }
    }

    std::vector<uint32_t> geography(static_cast<size_t>(n) * n);
    size_t grassCellCount = 0;
    size_t grassRenderableCellCount = 0;
    for (int z = 0; z < n; ++z)
    {
        for (int x = 0; x < n; ++x)
        {
            uint32_t cell = land.GetGeography(x, z).packed;
            const int layer = land.GetTexture(z, x);
            if (layer >= 0 && layer < static_cast<int>(grassLayer.size()) && grassLayer[static_cast<size_t>(layer)])
            {
                cell |= GrassTextureCell;
                ++grassCellCount;
            }
            // Match grass.wgsl: water, forests, roads/tracks and hard
            // buildings are excluded, but the legacy "full" flag is valid
            // ordinary ground on Everon and must remain grass-capable.
            if ((cell & GrassTextureCell) != 0 && (cell & 0x00000c7bu) == 0)
            {
                ++grassRenderableCellCount;
            }
            geography[static_cast<size_t>(z) * n + x] = cell;
        }
    }
    // Some legacy Everon WRP revisions mark a broad part of ordinary ground
    // as forest. If that leaves no possible grass anywhere, treat only that
    // blanket forest flag as invalid. Water, roads and tracks stay protected.
    // Real forest/builder exclusions remain intact whenever the source data
    // provides even one valid grass cell.
    if (grassCellCount > 0 && grassRenderableCellCount == 0)
    {
        for (uint32_t& cell : geography)
            cell &= ~0x00000018u; // forestInner | forestOuter
        for (uint32_t cell : geography)
        {
            if ((cell & GrassTextureCell) != 0 && (cell & 0x00000c7bu) == 0)
                ++grassRenderableCellCount;
        }
        LOG_WARN(Graphics,
                 "Wgpu grass: legacy geography had no usable grass cells; relaxed blanket forest flags -> {} cells",
                 grassRenderableCellCount);
    }
    // A handful of old WRP files also store hard-object density across entire
    // ground regions. Only as a final zero-candidate fallback do we relax it;
    // this is preferable to silently rendering no grass at all on the map.
    if (grassCellCount > 0 && grassRenderableCellCount == 0)
    {
        for (uint32_t& cell : geography)
            cell &= ~0x00000c00u; // howManyHardObjects
        for (uint32_t cell : geography)
        {
            if ((cell & GrassTextureCell) != 0 && (cell & 0x00000c7bu) == 0)
                ++grassRenderableCellCount;
        }
        LOG_WARN(
            Graphics,
            "Wgpu grass: legacy geography still had no usable cells; relaxed blanket hard-object flags -> {} cells",
            grassRenderableCellCount);
    }
    // What the shader will ACTUALLY place. grass.wgsl splits the mask: hard
    // exclusions (water/road/track/hard objects) always apply, forest only when
    // the compatibility override is off. `grassRenderableCellCount` above uses
    // the combined mask, so it under-reports whenever the override is active.
    size_t hardOnlyCells = 0;
    for (uint32_t cell : geography)
    {
        if ((cell & GrassTextureCell) != 0 && (cell & 0x00000c63u) == 0)
            ++hardOnlyCells;
    }
    LOG_INFO(Graphics,
             "Wgpu grass texture mask: {} layer(s), {} / {} surface cells, {} renderable after geography, "
             "{} with forest relaxed (road/water/building always excluded) [{}]",
             grassLayerCount, grassCellCount, geography.size(), grassRenderableCellCount, hardOnlyCells,
             grassLayerNames.empty() ? "none matched" : grassLayerNames);
    LOG_INFO(Graphics, "Wgpu terrain texture layers: [{}]", allLayerNames);
    wgr_grass_set_geography(_renderer, static_cast<uint32_t>(n), static_cast<uint32_t>(n), geography.data());
    UploadGrassTuft();
    UploadGrassBladeAtlas();
}

// GRS-E — upload an authored photographed grass clump for the mid LOD's crossed
// cards. Entirely OPTIONAL: its absence must change nothing except how the mid
// ring is drawn.
//
// If no texture is found, `have_tuft` stays false on the renderer side and the
// mid ring keeps its procedural crossed ribbons -- the long-standing look. That
// is the only fallback, deliberately. The game also ships a 2001 grass PAA
// (data/trava1_pmp2.pac), but its opaque texels average (0.322, 0.375, 0.334):
// blue level with red, i.e. grey-teal rather than green. Falling back to that
// would hand anyone without this asset a WORSE picture than no photo cards at
// all, so it is reachable only by pointing WGR_GRASS_TUFT at a converted copy.
void TerrainWgpu::UploadGrassTuft()
{
    if (!_renderer || _grassTuftUploaded)
    {
        return;
    }
    // Read straight off disk rather than the VFS: the file sits beside the
    // binaries, not in a PBO. stb_image is already vendored for the JPEG loader.
    const char* overridePath = std::getenv("WGR_GRASS_TUFT");
    const char* candidates[] = {
        overridePath,
        "assets/grass/meadow-grass-clump-alpha-1024.png",
    };
    for (const char* authored : candidates)
    {
        if (authored == nullptr || *authored == '\0')
        {
            continue;
        }
        int w = 0, h = 0, channels = 0;
        uint8_t* pixels = stbi_load(authored, &w, &h, &channels, 4); // force RGBA
        if (pixels == nullptr)
        {
            continue;
        }
        wgr_grass_set_tuft(_renderer, static_cast<uint32_t>(w), static_cast<uint32_t>(h), pixels);
        stbi_image_free(pixels);
        _grassTuftUploaded = true;
        LOG_INFO(Graphics, "Wgpu grass: mid-LOD clump texture uploaded ({}x{}, {} src channels) from '{}'", w, h,
                 channels, authored);
        return;
    }
    // Expected for anyone without the optional asset -- INFO, not a warning.
    LOG_INFO(Graphics, "Wgpu grass: no mid-LOD clump texture; mid ring uses procedural ribbons");
}

// GRS-F — replace only the near ribbon's procedural surface detail. The near
// grass keeps its existing per-blade geometry, density and wind; these are
// opaque texture layers, not alpha-cutout cards.
void TerrainWgpu::UploadGrassBladeAtlas()
{
    if (!_renderer || _grassBladeAtlasUploaded)
    {
        return;
    }
    constexpr std::array<const char*, 8> BladePaths = {
        "assets/grass/meadow-grass-blade-0.png", "assets/grass/meadow-grass-blade-1.png",
        "assets/grass/meadow-grass-blade-2.png", "assets/grass/meadow-grass-blade-3.png",
        "assets/grass/meadow-grass-blade-4.png", "assets/grass/meadow-grass-blade-5.png",
        "assets/grass/meadow-grass-blade-6.png", "assets/grass/meadow-grass-blade-7.png",
    };

    int width = 0;
    int height = 0;
    std::vector<uint8_t> layers;
    for (const char* path : BladePaths)
    {
        int w = 0;
        int h = 0;
        int channels = 0;
        uint8_t* pixels = stbi_load(path, &w, &h, &channels, 4); // force opaque RGBA upload
        if (pixels == nullptr || (width != 0 && (w != width || h != height)))
        {
            stbi_image_free(pixels);
            LOG_INFO(Graphics,
                     "Wgpu grass: no complete near-blade photo atlas; near ring uses procedural surface detail");
            return;
        }
        if (width == 0)
        {
            width = w;
            height = h;
            layers.reserve(static_cast<size_t>(width) * static_cast<size_t>(height) * 4u * BladePaths.size());
        }
        layers.insert(layers.end(), pixels, pixels + static_cast<size_t>(width) * static_cast<size_t>(height) * 4u);
        stbi_image_free(pixels);
    }

    wgr_grass_set_blade_atlas(_renderer, static_cast<uint32_t>(width), static_cast<uint32_t>(height),
                              static_cast<uint32_t>(BladePaths.size()), layers.data());
    _grassBladeAtlasUploaded = true;
    LOG_INFO(Graphics, "Wgpu grass: near-LOD photo blade atlas uploaded ({} layers, {}x{})", BladePaths.size(), width,
             height);
}

int TerrainWgpu::GrassSurfaceCount() const
{
    return static_cast<int>(_grassSurfaceNames.size());
}

const char* TerrainWgpu::GrassSurfaceName(int index) const
{
    return index >= 0 && index < GrassSurfaceCount() ? _grassSurfaceNames[static_cast<size_t>(index)].c_str() : "";
}

bool TerrainWgpu::GrassSurfaceEnabled(int index) const
{
    return index >= 0 && index < static_cast<int>(_grassSurfaceEnabled.size()) &&
           _grassSurfaceEnabled[static_cast<size_t>(index)];
}

void TerrainWgpu::SetGrassSurfaceEnabled(int index, bool enabled)
{
    if (index < 0 || index >= static_cast<int>(_grassSurfaceEnabled.size()) ||
        _grassSurfaceEnabled[static_cast<size_t>(index)] == enabled)
    {
        return;
    }
    _grassSurfaceEnabled[static_cast<size_t>(index)] = enabled;
    if (_uploaded != nullptr)
    {
        UploadGeography(*_uploaded);
    }
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

    // Refresh the coast wet-band params every frame: the animated sea level + clock (+ the
    // live-tuned coast look) drive the damp intertidal band, keyed on the SAME sea level and
    // swash the water uses so the two register at one moving waterline.
    const Engine::WaterSettings& coast = _engine.WaterLook();
    _params.sea_level = land.GetSeaLevel();
    _params.time = Glob.time.toFloat();
    _params.swash_speed = coast.swashSpeed;
    _params.swash_amp = coast.swashAmp;
    _params.wet_height = coast.wetHeight;
    _params.wet_darken = coast.wetDarken;
    wgr_terrain_set_params(_renderer, &_params);

    Camera* camera = scene.GetCamera();
    if (camera == nullptr)
    {
        return;
    }
    // The engine's visible rect (xBeg..zEnd) is the camera frustum projected to the
    // ground out to the far plane — already off-map-extending, not map-clamped. Map-only
    // clips it to the map (draw distance); extended clips it to the built tree so the
    // off-map border draws too. Either way frustum + CDLOD ranges + fog bound the result.
    const float landGrid = land.GetLandGrid();
    const int landRange = land.GetLandRange();
    float rx0, rz0, rx1, rz1;
    if (_extended)
    {
        rx0 = std::max(xBeg * landGrid, _treeMin);
        rz0 = std::max(zBeg * landGrid, _treeMin);
        rx1 = std::min(xEnd * landGrid, _treeMax);
        rz1 = std::min(zEnd * landGrid, _treeMax);
    }
    else
    {
        rx0 = std::max(xBeg, 0) * landGrid;
        rz0 = std::max(zBeg, 0) * landGrid;
        rx1 = std::min(xEnd, landRange) * landGrid;
        rz1 = std::min(zEnd, landRange) * landGrid;
    }

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
    // Off-map terrain draws in full, seabed included — NOT just above-water land. The
    // water is transparent, so the seabed shows through it; pruning below-sea off-map
    // would leave a visible seam at the map edge where in-map water sits over seabed
    // but off-map water sits over the empty sky background. (Once the water look plan
    // makes deep water opaque, an occlusion prune of fully-submerged off-map terrain
    // can come back as a pure optimization — keep the terrain extent >= the water
    // extent so seabed always underlies the ocean.)
    SelectVisibleCdlod(
        _tree, _rootIndex, _numLevels, _ranges, _morphRegion, *camera, rx0, rz0, rx1, rz1,
        [](const CdlodNode&) { return true; }, emit);

    _engine.SubmitTerrain(_selected);
    // The procedural path must remain available on maps that use the modern GPU
    // terrain path (which can skip the legacy alpha overlay callback entirely).
    // SubmitGrass is frame-deduplicated by EngineWgpu; SetGrassParams below is an
    // additional legacy signal, not the only activation path.
    _engine.SubmitGrass();
}

} // namespace Poseidon
