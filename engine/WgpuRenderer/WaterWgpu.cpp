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
#include <limits>
#include <cstring> // memcpy — packs the WTR freeze mask into WgrWaterParams.fft_control.z

namespace Poseidon
{
// CDLOD leaf span in terrain texels.  The GPU water mesh is intentionally denser
// (see water/mod.rs) so it can represent smooth FFT displacement inside this leaf.
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

struct ReferenceWaveMode
{
    float kx;
    float kz;
    float omega;
    float h0Real;
    float h0Imag;
    float displacementScale;
};

static std::array<float, 2> ReferenceHash(uint32_t x, uint32_t y)
{
    uint32_t h = y + 374761393u + x * 3266489917u;
    h = 2246822519u * (h ^ (h >> 15u));
    h = 3266489917u * (h ^ (h >> 13u));
    const uint32_t n = h ^ (h >> 16u);
    constexpr float invMax = 1.0f / 2147483647.0f;
    return {static_cast<float>((n >> 1u) & 0x7fffffffu) * invMax,
            static_cast<float>(((n * 48271u) >> 1u) & 0x7fffffffu) * invMax};
}

static float ReferenceSpreadNormalization(float s)
{
    constexpr float pi = 3.14159265358979323846f;
    if (s < 0.4f)
    {
        return 0.5f / pi + s * (0.220636f + s * (-0.109f + s * 0.090f));
    }
    const float a = std::sqrt(s);
    return (a * 0.5f + 0.0625f / a) / std::sqrt(pi);
}

static std::vector<ReferenceWaveMode> BuildReferenceWaveModes()
{
    struct AuthoredCascade
    {
        float length;
        float displacement;
        float windSpeed;
        float windDirection;
        float fetch;
        float swell;
        float spread;
        float detail;
    };
    constexpr float pi = 3.14159265358979323846f;
    constexpr std::array<AuthoredCascade, 2> cascades{{
        {88.0f, 1.0f, 10.0f, 20.0f * pi / 180.0f, 150000.0f, 0.8f, 0.2f, 1.0f},
        {57.0f, 0.75f, 5.0f, 15.0f * pi / 180.0f, 150000.0f, 0.8f, 0.4f, 1.0f},
    }};
    constexpr int resolution = 256;
    constexpr float gravity = 9.81f;
    constexpr float tau = 2.0f * pi;
    constexpr size_t retainedPerCascade = 4096;
    std::vector<ReferenceWaveMode> retained;
    retained.reserve(retainedPerCascade * cascades.size());

    for (const AuthoredCascade& cascade : cascades)
    {
        std::vector<ReferenceWaveMode> all;
        all.reserve(resolution * resolution);
        const float dk = tau / cascade.length;
        const float alpha =
            0.076f * std::pow(cascade.windSpeed * cascade.windSpeed / (cascade.fetch * gravity), 0.22f);
        const float peak =
            22.0f * std::pow(gravity * gravity / (cascade.windSpeed * cascade.fetch), 1.0f / 3.0f);

        for (int y = 0; y < resolution; ++y)
        {
            for (int x = 0; x < resolution; ++x)
            {
                const float kx = (static_cast<float>(x) - resolution * 0.5f) * dk;
                const float kz = (static_cast<float>(y) - resolution * 0.5f) * dk;
                const float k = std::sqrt(kx * kx + kz * kz) + 1e-6f;
                const float kd = k * 20.0f;
                const float tanhKd = std::tanh(kd);
                const float omega = std::sqrt(gravity * k * tanhKd);
                const float derivative =
                    0.5f * gravity * (tanhKd + kd * (1.0f - tanhKd * tanhKd)) / omega;
                const float p = omega / peak;
                const float s = omega <= peak
                                    ? 6.97f * std::pow(std::abs(p), 4.06f)
                                    : 9.77f * std::pow(std::abs(p),
                                                       -2.33f - 1.45f *
                                                                    (cascade.windSpeed * peak / gravity - 1.17f));
                const float sx = 16.0f * std::tanh(peak / omega) * cascade.swell * cascade.swell;
                const float alignment =
                    (kx * std::sin(cascade.windDirection) + kz * std::cos(cascade.windDirection)) / k;
                const float directional = ReferenceSpreadNormalization(s + sx) *
                                          std::pow(std::max(0.5f * (1.0f + alignment), 0.0f), s + sx);
                const float spread =
                    directional * (1.0f - cascade.spread) + (0.5f / pi) * cascade.spread;
                const float sigma = omega <= peak ? 0.07f : 0.09f;
                const float r = std::exp(-(omega - peak) * (omega - peak) /
                                         (2.0f * sigma * sigma * peak * peak));
                const float jonswap = alpha * gravity * gravity / std::pow(omega, 5.0f) *
                                      std::exp(-1.25f * std::pow(peak / omega, 4.0f)) *
                                      std::pow(3.3f, r);
                const float wh = std::min(omega * std::sqrt(20.0f / gravity), 2.0f);
                const float attenuation =
                    wh <= 1.0f ? 0.5f * wh * wh : 1.0f - 0.5f * (2.0f - wh) * (2.0f - wh);
                const float damping =
                    std::exp(-(1.0f - cascade.detail) * (1.0f - cascade.detail) * k * k);
                const float variance =
                    jonswap * attenuation * spread * damping * derivative / k * dk * dk;
                const auto uniform = ReferenceHash(static_cast<uint32_t>(x), static_cast<uint32_t>(y));
                const float radius = std::sqrt(-2.0f * std::log(std::max(uniform[0], 1e-7f)));
                const float theta = tau * uniform[1];
                const float amplitude = std::sqrt(std::max(2.0f * variance, 0.0f));
                all.push_back({kx, kz, omega, radius * std::cos(theta) * amplitude,
                               radius * std::sin(theta) * amplitude, cascade.displacement});
            }
        }
        const size_t count = std::min(retainedPerCascade, all.size());
        std::partial_sort(all.begin(), all.begin() + count, all.end(),
                          [](const ReferenceWaveMode& a, const ReferenceWaveMode& b)
                          {
                              return std::hypot(a.h0Real, a.h0Imag) * a.displacementScale >
                                     std::hypot(b.h0Real, b.h0Imag) * b.displacementScale;
                          });
        retained.insert(retained.end(), all.begin(), all.begin() + count);
    }
    return retained;
}

static float ReferenceSurfaceHeight(float x, float z, float time, float amplitude, float speed,
                                    float wavelengthScale)
{
    static const std::vector<ReferenceWaveMode> modes = BuildReferenceWaveModes();
    const float invScale = 1.0f / std::max(wavelengthScale, 0.01f);
    float height = 0.0f;
    for (const ReferenceWaveMode& mode : modes)
    {
        const float phase = mode.omega * time * speed + (mode.kx * x + mode.kz * z) * invScale;
        height += 2.0f * (mode.h0Real * std::cos(phase) - mode.h0Imag * std::sin(phase)) *
                  mode.displacementScale;
    }
    return height * std::max(amplitude, 0.0f);
}

// The spectrum consumes these independently per layer.  Keeping all preset data here
// makes a live Water-tab switch atomic: lengths in WgrWaterParams and their matching
// wind/fetch/seed/scales reach the GPU in the same frame.
static void ApplyCascadePreset(WgrRenderer* renderer, int preset)
{
    WgrWaterCascadeConfig c{};
    c.enabled = 1;
    // Match GodotOceanWaves Water.map_size and the renderer allocation in fft.rs.
    c.resolution = 1024;
    c.displacement_scale = 1.0f;
    c.horiz_displacement_scale = 1.0f;
    c.normal_scale = 1.0f;
    c.foam_scale = 1.0f;
    c.wind_speed = 10.0f;
    c.wind_direction_rad = 0.349f;
    c.fetch_meters = 150000.0f;
    c.water_depth_meters = 20.0f;
    c.swell = 0.80f;
    c.directional_spread = 0.20f;
    c.short_wave_detail = 1.0f;
    c.whitecap_threshold = 0.50f;
    c.spectrum_seed = 0;
    // Godot water.gd dispatches cascade i at 120 + PI*i seconds.
    c.phase_offset_seconds = 120.0f;
    c.update_rate_hz = 60.0f;

    if (preset == 1)
    {
        // krautdev/GodotOceanWaves' three published cascades.
        c.tile_length_x = c.tile_length_y = 88.0f;
        WgrWaterCascadeConfig b = c;
        b.tile_length_x = b.tile_length_y = 57.0f;
        b.displacement_scale = b.horiz_displacement_scale = 0.75f;
        b.foam_scale = 0.0f;
        b.wind_speed = 5.0f;
        b.wind_direction_rad = 0.2618f;
        b.directional_spread = 0.40f;
        b.spectrum_seed = 0;
        b.phase_offset_seconds = 120.0f + 3.14159265359f;
        WgrWaterCascadeConfig d = c;
        d.tile_length_x = d.tile_length_y = 16.0f;
        d.displacement_scale = d.horiz_displacement_scale = 0.0f;
        d.normal_scale = 0.25f;
        d.foam_scale = 3.0f;
        d.wind_speed = 20.0f;
        d.fetch_meters = 550000.0f;
        d.directional_spread = 0.40f;
        d.whitecap_threshold = 0.25f;
        d.spectrum_seed = 0;
        d.phase_offset_seconds = 120.0f + 2.0f * 3.14159265359f;
        c.foam_scale = 8.0f;
        WgrWaterCascadeConfig disabled{};
        wgr_water_set_cascade_config(renderer, 0, &c);
        wgr_water_set_cascade_config(renderer, 1, &b);
        wgr_water_set_cascade_config(renderer, 2, &d);
        wgr_water_set_cascade_config(renderer, 3, &disabled);
        return;
    }

    if (preset == 2)
    {
        // Retained solely for visual A/B of the old harmonic implementation.
        c.tile_length_x = c.tile_length_y = 48.0f;
        WgrWaterCascadeConfig b = c; b.tile_length_x = b.tile_length_y = 144.0f; b.wind_speed = 8.5f; b.spectrum_seed = 5678; b.phase_offset_seconds += 3.14159265359f;
        WgrWaterCascadeConfig d = c; d.tile_length_x = d.tile_length_y = 432.0f; d.wind_speed = 7.0f; d.spectrum_seed = 91011; d.phase_offset_seconds += 2.0f * 3.14159265359f;
        WgrWaterCascadeConfig e = c; e.tile_length_x = e.tile_length_y = 1296.0f; e.wind_speed = 6.0f; e.spectrum_seed = 121314; e.phase_offset_seconds += 3.0f * 3.14159265359f;
        wgr_water_set_cascade_config(renderer, 0, &c);
        wgr_water_set_cascade_config(renderer, 1, &b);
        wgr_water_set_cascade_config(renderer, 2, &d);
        wgr_water_set_cascade_config(renderer, 3, &e);
        return;
    }

    // Production: larger pairwise-prime domains make an individual cascade's repeat
    // imperceptible as well as pushing the shared period beyond the playable world.
    // Small per-layer direction/seed changes prevent the bands from phase-locking.
    c.tile_length_x = c.tile_length_y = 97.0f;
    c.wind_speed = 12.0f;
    c.wind_direction_rad = 0.31f;
    c.fetch_meters = 210000.0f;
    c.spectrum_seed = 1471;
    WgrWaterCascadeConfig b = c; b.tile_length_x = b.tile_length_y = 257.0f; b.wind_speed = 9.5f; b.wind_direction_rad = 0.43f; b.spectrum_seed = 8623; b.phase_offset_seconds += 3.14159265359f;
    WgrWaterCascadeConfig d = c; d.tile_length_x = d.tile_length_y = 683.0f; d.wind_speed = 7.5f; d.wind_direction_rad = 0.22f; d.fetch_meters = 350000.0f; d.spectrum_seed = 24593; d.phase_offset_seconds += 2.0f * 3.14159265359f;
    WgrWaterCascadeConfig e = c; e.tile_length_x = e.tile_length_y = 1777.0f; e.wind_speed = 6.0f; e.wind_direction_rad = 0.37f; e.fetch_meters = 550000.0f; e.spectrum_seed = 73471; e.phase_offset_seconds += 3.0f * 3.14159265359f;
    wgr_water_set_cascade_config(renderer, 0, &c);
    wgr_water_set_cascade_config(renderer, 1, &b);
    wgr_water_set_cascade_config(renderer, 2, &d);
    wgr_water_set_cascade_config(renderer, 3, &e);
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
    // Match the energetic reference sea state.  The last lane drives spectral energy;
    // 0.08 made waves, foam, and spray effectively invisible in normal gameplay.
    _params.fft_wind_sea = {0.82f, 0.57f, 12.0f, 0.65f};
    _params.fft_cascade_lengths = {48.0f, 144.0f, 432.0f, 1296.0f};
    // The sole draw path is the global ocean plane. Keep directed flow disabled until
    // water-body batches can supply a river-only material signal.
    _params.flow_direction_speed = {0.0f, 0.0f, 0.0f, static_cast<float>(WGR_WATER_KIND_OCEAN)};
    _haveInteractionDomain = false;

    ApplyCascadePreset(_renderer, _engine.WaterLook().cascadePreset);
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
    const Vector3 cameraPos = camera->Position();
    // Post-processing is a camera effect.  Player-body water depth is useful for
    // splash events, but using it here made wading apply underwater fog above water.
    float localSurface = land.GetSeaLevel();
    if (look.cascadePreset == 1)
    {
        localSurface += ReferenceSurfaceHeight(cameraPos.X(), cameraPos.Z(), _params.time,
                                               look.waveAmp, look.waveSpeed, look.waveScale);
    }
    // Small asymmetric hysteresis keeps the compositor from flickering when the eye
    // rides exactly on a moving FFT crest.
    if (_cameraSubmerged)
    {
        _cameraSubmerged = cameraPos.Y() < localSurface + 0.08f;
    }
    else
    {
        _cameraSubmerged = cameraPos.Y() < localSurface - 0.03f;
    }
    _params.fft_control.w = _cameraSubmerged ? 1.0f : 0.0f;
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
        // Production non-repeating four-cascade ocean.  The smallest 97m domain is
        // large enough that its own pattern does not read as a visible tiled square.
        _params.fft_cascade_lengths = {97.0f, 257.0f, 683.0f, 1777.0f};
    }
    ApplyCascadePreset(_renderer, look.cascadePreset);

    // y gates the GPU whitewater/spray billboard pass and z controls its activity.
    // It is deliberately off by default: ordinary rifle impacts should keep their
    // ripple without spawning a large particle field. x remains the debug-view selector.
    _params.debug_params = {static_cast<float>(look.debugView), look.rifleImpactSpray ? 1.0f : 0.0f,
                            look.waterSplashParticleActivity, 0.0f};
    // Runtime proof that the Water tab reaches the actual renderer. This is deliberately
    // edge-triggered: one log row per edited amplitude, not one row per frame.
    static float lastLoggedWaveAmp = -1.0f;
    if (std::abs(lastLoggedWaveAmp - look.waveAmp) > 0.0001f)
    {
        LOG_INFO(Graphics, "Water look applied: amplitude={:.3f}, choppiness={:.3f}, speed={:.3f}, preset={}",
                 look.waveAmp, look.waveChoppy, look.waveSpeed, look.cascadePreset);
        lastLoggedWaveAmp = look.waveAmp;
    }
    wgr_water_set_params(_renderer, &_params);

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
        // Find the closest dry/very-shallow terrain sample around this water tile.
        // The result is deliberately tile-local: it bends only a short coastal breaker
        // train, never the globally coherent FFT sea state offshore.
        const float centreX = s.originX + s.size * 0.5f;
        const float centreZ = s.originZ + s.size * 0.5f;
        const int centreIx = static_cast<int>(std::lround(centreX / land.GetTerrainGrid()));
        const int centreIz = static_cast<int>(std::lround(centreZ / land.GetTerrainGrid()));
        const int terrainRange = land.GetTerrainRange();
        float bestDistance = 1.0e9f;
        float dirX = 0.0f;
        float dirZ = 0.0f;
        float localDepth = 1000.0f;
        if (centreIx >= 0 && centreIz >= 0 && centreIx < terrainRange && centreIz < terrainRange)
        {
            localDepth = std::max(land.GetSeaLevel() - land.GetHeight(centreIz, centreIx), 0.0f);
            constexpr int directions[8][2] = {{1,0},{-1,0},{0,1},{0,-1},{1,1},{1,-1},{-1,1},{-1,-1}};
            for (const auto& d : directions)
            {
                for (int step = 2; step <= 24; step += 2)
                {
                    const int x = centreIx + d[0] * step;
                    const int z = centreIz + d[1] * step;
                    if (x < 0 || z < 0 || x >= terrainRange || z >= terrainRange)
                    {
                        break;
                    }
                    if (land.GetHeight(z, x) >= land.GetSeaLevel() - 0.15f)
                    {
                        const float dx = static_cast<float>(d[0] * step) * land.GetTerrainGrid();
                        const float dz = static_cast<float>(d[1] * step) * land.GetTerrainGrid();
                        const float distance = std::sqrt(dx * dx + dz * dz);
                        if (distance < bestDistance)
                        {
                            bestDistance = distance;
                            dirX = dx / distance;
                            dirZ = dz / distance;
                        }
                        break;
                    }
                }
            }
        }
        const float shoreDistanceFade = std::clamp((48.0f - bestDistance) / 36.0f, 0.0f, 1.0f);
        const float shallowFade = std::clamp((8.0f - localDepth) / 6.0f, 0.0f, 1.0f);
        node.shore_direction = {dirX, dirZ};
        node.shore_factor = shoreDistanceFade * shallowFade;
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
