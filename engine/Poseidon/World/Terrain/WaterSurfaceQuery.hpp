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

// Evaluates a compact, stable open-ocean spectrum at world X/Z. seaLevel is supplied by the
// landscape.
//
// The band amplitudes, animation rate and wavelengths are scaled by the live renderer water look
// (amplitude / speed / scale) so a unit floating on the sea rides the same sea state that is being
// drawn. Previously every constant here was hardcoded — a fixed 0.08 sea state giving 16-30 cm
// components, on the superseded 48/144/432/1296 m cascade bands — so buoyancy bobbed by a couple of
// centimetres no matter how rough the rendered ocean was, and never agreed with it.
//
// It remains an approximation: this is a six-component Gerstner sum, not the GPU FFT field, and it
// deliberately samples no GPU texture so it stays deterministic and available on the server. It
// matches the rendered sea in scale, period and roughness rather than crest for crest.
WaterSurfaceSample QueryWaterSurface(float worldX, float worldZ, float time, float seaLevel);

} // namespace Poseidon
