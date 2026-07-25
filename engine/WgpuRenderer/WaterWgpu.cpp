#include "WaterWgpu.hpp"

#include "CdlodDriver.hpp"
#include "EngineWgpu.hpp"

#include <Poseidon/Core/Global.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Graphics/Rendering/WaterInteractionBridge.hpp>
#include <Poseidon/World/Scene/Camera/Camera.hpp>
#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/World/Terrain/Landscape.hpp>
#include <Poseidon/World/Terrain/LandscapeShared.hpp>

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring> // memcpy — packs the WTR freeze mask into WgrWaterParams.fft_control.z

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
    _interactionDemo = EnvFloat("WGR_WATER_INTERACTION_DEMO", 0.0f) > 0.5f;
}

void WaterWgpu::BuildQuadtree(const Landscape& land)
{
    const int range = land.GetTerrainRange();
    const float grid = land.GetTerrainGrid();

    // Over-size the root and centre the map inside it so the ocean continues past the
    // map edges to the horizon (the legacy path fills off-map with all-water tiles).
    const int coverage = std::max(range, static_cast<int>(std::lround(_extentFactor * range)));
    const int rootTexels = CdlodRootTexels(coverage, WaterGridN);
    const int originTexel = CdlodCenteredOrigin(rootTexels, range, WaterGridN);

    // Same leaf min/max scan as terrain where the leaf overlaps the map (water
    // existence is defined by the terrain dipping below sea level); off-map texels are
    // open ocean. Their bound is pinned to the sea datum (0), NOT a fake deep seabed:
    // the node minY/maxY drive both the below-sea keep test AND the CDLOD frustum/LOD
    // bounding volume, and the surface is drawn at sea level regardless of any seabed —
    // a deep off-map value would sink the bounding sphere below the horizon view and
    // wrongly cull near off-map tiles. 0 <= the keep threshold, so off-map is kept.
    // WTR-034 — Conservative CDLOD displacement bounds:
    // Derivation of conservative bounding volume expansion:
    // 1. Vertical displacement bound (D_y): sum of FFT cascade max crest heights (wave_amp * 1.8f)
    // 2. Horizontal choppiness bound (D_xz): horizontal displacement shifts vertices by up to choppiness * wave_amp (1.2f * wave_amp)
    // 3. Interaction & particle impulse padding: maximum vessel/interaction splash impulse height (+/- 1.5m)
    // 4. Safety margin (1.25x): guarantees bounding spheres never cull crests near frustum edges.
    constexpr float OffMapSurface = 0.0f;
    const float vertDisplacement = _params.wave_amp * 1.8f;
    const float horizChoppiness = _params.wave_amp * 1.2f;
    const float interactionImpulse = 1.5f;
    const float conservativeBound = (vertDisplacement + horizChoppiness * 0.5f + interactionImpulse) * 1.25f;
    const float crestPadding = std::max(conservativeBound, 3.5f);
    auto leafBounds = [&](int ox, int oz, int span, float& mn, float& mx)
    {
        for (int z = oz; z <= oz + span; z++)
        {
            for (int x = ox; x <= ox + span; x++)
            {
                const float h =
                    (x < 0 || z < 0 || x >= range || z >= range) ? OffMapSurface : land.GetHeight(z, x);
                mn = std::min(mn, h - crestPadding);
                mx = std::max(mx, h + crestPadding);
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
    // Weather does not currently expose a renderer-facing wind vector. Keep this deterministic
    // until the environment weather service is threaded here.
    _params.fft_control = {1.0f, 1337.0f, 12.0f, 0.0f};
    _params.fft_wind_sea = {0.82f, 0.57f, 6.0f, 0.08f};
    _params.fft_cascade_lengths = {48.0f, 144.0f, 432.0f, 1296.0f};
    // The sole draw path is the global ocean plane. Keep directed flow disabled until
    // water-body batches can supply a river-only material signal.
    _params.flow_direction_speed = {0.0f, 0.0f, 0.0f, static_cast<float>(WGR_WATER_KIND_OCEAN)};
    _haveInteractionDomain = false;
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
    const Engine::WaterSettings& look = _engine.WaterLook();
    if (!look.enabled)
    {
        // Water suppressed from the tab: draw nothing (the seabed shows, for A/B).
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

    // WTR-001 — deterministic water debug. All freeze switches substitute a fixed value for the
    // variable the shader reads, so the same test frame produces the same UBO (and the same
    // h0/random stream) every launch. Glob.time itself is NOT mutated — only the value handed to
    // the water/cloud/underwater shaders — so gameplay and net time are untouched.
    const Engine::WaterSettings::Freeze& fz = look.freeze;

    // Refresh the animated sea level + wave clock + live look every frame; the whole
    // plane rides at this height (no mesh regeneration — the legacy path re-levelled
    // cached vertices), and the Gerstner waves advance off `time` in the shader.
    _params.sea_level = land.GetSeaLevel();
    _params.time = fz.freezeTime ? fz.fixedTime : Glob.time.toFloat();
    _params.wave_amp = look.waveAmp;
    _params.wave_choppy = look.waveChoppy;
    _params.wave_speed = look.waveSpeed;
    _params.wave_scale = look.waveScale;
    _params.fade_start = look.fadeStart;
    _params.fade_end = look.fadeEnd;
    _params.warp_amp = look.warpAmp;
    _params.spec_power = look.specPower;
    _params.spec_intensity = look.specIntensity;
    _params.alpha = look.alpha;
    _params.shadow_dim = look.shadowDim;
    _params.color_ext = look.colorExt;
    _params.coast_fade = look.coastFade;
    _params.shallow_color = {look.shallowColor[0], look.shallowColor[1], look.shallowColor[2], 0.0f};
    _params.deep_color = {look.deepColor[0], look.deepColor[1], look.deepColor[2], 0.0f};
    _params.foam_width = look.foamWidth;
    _params.foam_intensity = look.foamIntensity;
    _params.swash_amp = look.swashAmp;
    _params.swash_speed = look.swashSpeed;
    // The fourth FFT control component is otherwise padding. The renderer receives no
    // reliable head transform from legacy infantry, so use deep player immersion plus
    // a downward look direction as the visual-only submersion signal.
    _params.fft_control.w = GetPlayerWaterDepth() > 0.80f && camera->Direction().Y() < -0.20f ? 1.0f : 0.0f;
    // WTR-001 — deterministic FFT seed. The authored default (1337.0, set in BuildQuadtree)
    // already keeps the random field stable across frames; allow the dev tab to override it,
    // so a frozen frame is reproducible regardless of seq-of-edits to the spectrum. Setting a
    // non-negative value rewrites fft_control[1]; -1 keeps the authored 1337 (no swap).
    if (fz.fftSeed >= 0)
    {
        _params.fft_control.y = static_cast<float>(fz.fftSeed);
    }
    // WTR-001 — freeze dispatch mask packed into fft_control.z. The Rust side reads the float's
    // bits as the WGR_WATER_FREEZE_* mask and skips the matching compute dispatch. Encoding is
    // bit-cast so the shaders still see a normal IEEE float (0.0 with no bits = no freeze).
    uint32_t freezeMask = 0u;
    if (fz.freezeFft) { freezeMask |= WGR_WATER_FREEZE_FFT; }
    if (fz.freezeInteraction) { freezeMask |= WGR_WATER_FREEZE_INTERACTION; }
    if (fz.freezeFoam) { freezeMask |= WGR_WATER_FREEZE_FOAM; }
    float freezeBits = 0.0f;
    std::memcpy(&freezeBits, &freezeMask, sizeof(freezeBits));
    _params.fft_control.z = freezeBits;
    // WTR-036C / WTR-037 — Apply cascade configuration presets
    if (look.cascadePreset == 1)
    {
        // GodotOceanWaves Reference Style (3 cascades: 88m, 57m, 16m)
        _params.fft_cascade_lengths = {88.0f, 57.0f, 16.0f, 0.0f};
    }
    else if (look.cascadePreset == 2)
    {
        // Legacy 4-Cascade Harmonic (48m, 144m, 432m, 1296m)
        _params.fft_cascade_lengths = {48.0f, 144.0f, 432.0f, 1296.0f};
    }
    else
    {
        // WTR-037 Production Non-Harmonic 4-Cascade (37m, 89m, 211m, 503m - >50 km repeat period)
        _params.fft_cascade_lengths = {37.0f, 89.0f, 211.0f, 503.0f};
    }

    _params.debug_params = {static_cast<float>(look.debugView), 0.0f, 0.0f, 0.0f};
    wgr_water_set_params(_renderer, &_params);

    const Vector3 cameraPos = camera->Position();
    // WTR-001 — repeatable-camera-path foundation. When the Water tab sets a frame tag (>= 0),
    // log it with an FNV-1a digest of the exact UBO bytes just uploaded plus the camera pose,
    // so two launches can be diffed frame-by-frame from the log alone (the acceptance evidence
    // for "the same test frame should reproduce the same result between launches").
    if (fz.cameraPathFrame >= 0)
    {
        uint32_t digest = 2166136261u; // FNV-1a 32-bit offset basis
        const auto* bytes = reinterpret_cast<const unsigned char*>(&_params);
        for (size_t i = 0; i < sizeof(_params); ++i)
        {
            digest = (digest ^ bytes[i]) * 16777619u;
        }
        LOG_INFO(Graphics,
                 "WTR-001 camPath frame={} waterUboDigest={:08x} cam=({:.3f},{:.3f},{:.3f}) dir=({:.3f},{:.3f},{:.3f})",
                 fz.cameraPathFrame, digest, cameraPos.X(), cameraPos.Y(), cameraPos.Z(),
                 camera->Direction().X(), camera->Direction().Y(), camera->Direction().Z());
    }
    constexpr float interactionSize = 256.0f;
    const float originX = std::floor((cameraPos.X() - interactionSize * 0.5f) / 4.0f) * 4.0f;
    const float originZ = std::floor((cameraPos.Z() - interactionSize * 0.5f) / 4.0f) * 4.0f;
    const float now = _params.time;
    // WTR-063 — Fixed simulation timestep accumulator:
    // Accumulates frame dt and executes sub-steps of fixed 1/60s (0.016666s) to ensure wave physics stability.
    static float s_interactionAccumulator = 0.0f;
    constexpr float kFixedStep = 1.0f / 60.0f;
    const float rawDt = fz.freezeInteraction ? 0.0f : (fz.fixedDelta > 0.0f ? fz.fixedDelta : (now - _lastInteractionTime));
    s_interactionAccumulator += std::clamp(rawDt, 0.0f, 0.1f);
    float dt = kFixedStep;
    if (s_interactionAccumulator >= kFixedStep)
    {
        s_interactionAccumulator -= kFixedStep;
    }
    else if (rawDt == 0.0f)
    {
        dt = 0.0f;
    }

    std::array<WgrWaterInteractionEvent, WGR_MAX_WATER_INTERACTIONS> events{};
    uint32_t eventCount = 0;
    if (_interactionDemo)
    {
        const int pulse = static_cast<int>(std::floor(now / 3.0f));
        if (pulse != _lastInteractionDemoPulse)
        {
            const Vector3 direction = camera->Direction();
            WgrWaterInteractionEvent& event = events[eventCount++];
            event.position_radius = {cameraPos.X() + direction.X() * 14.0f, cameraPos.Z() + direction.Z() * 14.0f,
                                     1.7f, 0.30f};
            event.velocity_kind = {direction.X() * 2.0f, direction.Z() * 2.0f, -4.0f,
                                   static_cast<float>(WGR_WATER_INTERACTION_OBJECT)};
            event.time_life_foam_mass = {0.0f, 1.6f, 0.35f, 0.0f};
            event.direction_depth_flags = {direction.X(), direction.Z(), 0.0f,
                                           static_cast<float>(WGR_WATER_INTERACTION_PENDING_IMPULSE)};
            _lastInteractionDemoPulse = pulse;
        }
    }
    std::array<HydroWaterInteractionEvent, HydroMaxWaterInteractions> submitted{};
    const uint32_t submittedCount = DrainWaterInteractions(submitted.data(), HydroMaxWaterInteractions);
    for (uint32_t i = 0; i < submittedCount && eventCount < WGR_MAX_WATER_INTERACTIONS; ++i)
    {
        const HydroWaterInteractionEvent& source = submitted[i];
        WgrWaterInteractionEvent& event = events[eventCount++];
        event.position_radius = {source.positionRadius[0], source.positionRadius[1], source.positionRadius[2],
                                 source.positionRadius[3]};
        event.velocity_kind = {source.velocityKind[0], source.velocityKind[1], source.velocityKind[2],
                               source.velocityKind[3]};
        event.time_life_foam_mass = {source.timeLifeFoamMass[0], source.timeLifeFoamMass[1],
                                     source.timeLifeFoamMass[2], source.timeLifeFoamMass[3]};
        event.direction_depth_flags = {source.directionDepthFlags[0], source.directionDepthFlags[1],
                                       source.directionDepthFlags[2], source.directionDepthFlags[3]};
    }

    const bool reset = !_haveInteractionDomain || std::abs(originX - _interaction.domain.x) > interactionSize * 0.5f || std::abs(originZ - _interaction.domain.y) > interactionSize * 0.5f;
    _interaction.previous_domain = _haveInteractionDomain ? _interaction.domain : WgrVec4{originX, originZ, interactionSize, 1.0f / interactionSize};
    _interaction.domain = {originX, originZ, interactionSize, 1.0f / interactionSize};
    _interaction.grid = {256.0f, dt, static_cast<float>(eventCount), reset ? 1.0f : 0.0f};
    _interaction.physics = {12.0f, 1.6f, 0.35f, 1.2f};
    _interaction.misc = {0.0f, now, 0.0f, 0.0f};
    _interaction.weather = {0.0f, std::clamp(1.0f - look.waveAmp * 0.45f, 0.15f, 1.0f), 0.0f, 0.0f};
    wgr_water_set_interaction_params(_renderer, &_interaction);

    if (eventCount != 0)
    {
        wgr_water_submit_interactions(_renderer, events.data(), eventCount);
    }
    _lastInteractionTime = now;
    _haveInteractionDomain = true;

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
