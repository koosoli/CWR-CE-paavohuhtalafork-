#pragma once

namespace Poseidon
{

// Tier-A CPU predictor for gameplay. It deliberately does not sample GPU FFT
// textures, so it is deterministic and is only an approximation of rendered water.
struct WaterSurfaceSample
{
    float height;
    float normalX;
    float normalY;
    float normalZ;
    float velocityX;
    float velocityZ;
    float roughness;
};

// Evaluates a compact, stable open-ocean spectrum at world X/Z. The components are
// derived from the WGPU water defaults: wind (0.82, 0.57), speed 6, sea state 0.08,
// and the 48/144/432/1296 m cascade bands. seaLevel is supplied by the landscape.
WaterSurfaceSample QueryWaterSurface(float worldX, float worldZ, float time, float seaLevel);

} // namespace Poseidon
