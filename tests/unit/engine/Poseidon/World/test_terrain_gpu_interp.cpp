#include <catch2/catch_approx.hpp>
#include <catch2/catch_test_macros.hpp>

#include <Poseidon/World/Terrain/LandscapeLod.hpp>

#include <algorithm>
#include <cmath>
#include <vector>

using namespace Poseidon;

// Pins the wgpu terrain vertex shader's height sampling (terrain.wgsl
// `sample_height`) to the engine's CPU reference. The shader must reproduce
// Landscape::SurfaceY / LandscapeLod::InterpolateHeight exactly (per-cell
// triangle interpolation, not hardware bilinear) so decals routed OnSurface stay
// coplanar with the GPU-rendered surface. If InterpolateHeight ever changes,
// this test fails as a reminder to update the shader in lockstep.

namespace
{
// CPU mirror of terrain.wgsl `sample_height`: world-xz -> heightmap texel space
// (floor toward the cell origin), then the same triangle pick as SurfaceY.
// `load(ix, iz)` clamps and returns the sample, mirroring `hm_load`.
template <typename Load>
float ShaderSampleHeight(float worldX, float worldZ, float terrainGrid, Load&& load)
{
    const float tx = worldX / terrainGrid;
    const float tz = worldZ / terrainGrid;
    const int ix = static_cast<int>(std::floor(tx));
    const int iz = static_cast<int>(std::floor(tz));
    const float fx = tx - ix;
    const float fz = tz - iz;

    const float y00 = load(ix, iz);
    const float y01 = load(ix + 1, iz);
    const float y10 = load(ix, iz + 1);
    const float y11 = load(ix + 1, iz + 1);

    if (fx <= 1.0f - fz)
    {
        return y00 + (y10 - y00) * fz + (y01 - y00) * fx;
    }
    return y10 + (y01 - y11) - (y10 - y11) * fx - (y01 - y11) * fz;
}
} // namespace

TEST_CASE("terrain shader triangle interp matches InterpolateHeight", "[terrain][graphics]")
{
    // Deterministic pseudo-random corner heights + fractional positions.
    unsigned int state = 0x1234567u;
    auto rnd = [&state]() -> float
    {
        state = state * 1664525u + 1013904223u;
        return static_cast<float>(state >> 8) / static_cast<float>(1u << 24); // [0,1)
    };

    for (int i = 0; i < 2000; i++)
    {
        const float y00 = rnd() * 100.0f - 50.0f;
        const float y01 = rnd() * 100.0f - 50.0f;
        const float y10 = rnd() * 100.0f - 50.0f;
        const float y11 = rnd() * 100.0f - 50.0f;
        const float fx = rnd();
        const float fz = rnd();

        // Shader branch (fx = xIn, fz = zIn) vs the engine reference.
        float shader;
        if (fx <= 1.0f - fz)
        {
            shader = y00 + (y10 - y00) * fz + (y01 - y00) * fx;
        }
        else
        {
            shader = y10 + (y01 - y11) - (y10 - y11) * fx - (y01 - y11) * fz;
        }
        const float ref = InterpolateHeight(fx, fz, y00, y01, y10, y11);
        REQUIRE(shader == Catch::Approx(ref).epsilon(1e-6));
    }
}

TEST_CASE("terrain shader height sampling is exact at grid points", "[terrain][graphics]")
{
    // A 4x4 heightmap; sampling exactly on a texel must return that texel.
    constexpr int N = 4;
    std::vector<float> h(N * N);
    for (int z = 0; z < N; z++)
    {
        for (int x = 0; x < N; x++)
        {
            h[z * N + x] = static_cast<float>(x * 10 + z);
        }
    }
    auto load = [&](int ix, int iz) -> float
    {
        ix = std::clamp(ix, 0, N - 1);
        iz = std::clamp(iz, 0, N - 1);
        return h[iz * N + ix];
    };

    const float terrainGrid = 5.0f;
    for (int z = 0; z < N; z++)
    {
        for (int x = 0; x < N; x++)
        {
            const float y = ShaderSampleHeight(x * terrainGrid, z * terrainGrid, terrainGrid, load);
            REQUIRE(y == Catch::Approx(h[z * N + x]));
        }
    }
}

TEST_CASE("terrain shader height sampling interpolates along a cell edge", "[terrain][graphics]")
{
    // Two adjacent samples of known height; a point halfway between them along x
    // (z on the grid line) must be their average.
    constexpr int N = 2;
    std::vector<float> h = {0.0f, 20.0f, 0.0f, 20.0f}; // row-major, z*N + x
    auto load = [&](int ix, int iz) -> float
    {
        ix = std::clamp(ix, 0, N - 1);
        iz = std::clamp(iz, 0, N - 1);
        return h[iz * N + ix];
    };

    const float terrainGrid = 10.0f;
    const float y = ShaderSampleHeight(0.5f * terrainGrid, 0.0f, terrainGrid, load);
    REQUIRE(y == Catch::Approx(10.0f));
}
