#include <Poseidon/World/Terrain/WaterSurfaceQuery.hpp>

#include <cmath>

namespace Poseidon
{
namespace
{
constexpr float Pi2 = 6.28318530718f;
constexpr float Gravity = 9.81f;
constexpr float WindX = 0.82f;
constexpr float WindZ = 0.57f;
constexpr float WindSpeed = 6.0f;
constexpr float SeaState = 0.08f;
constexpr float SpectrumSeed = 1337.0f;
constexpr float WindLength = 0.998498873f;

struct WaveComponent
{
    float directionX;
    float directionZ;
    float length;
    float amplitude;
    float seedPhase;
};

// Long components carry the four renderer cascade scales; the two neighbouring bands
// avoid an overly regular single-direction swell while staying below FFT detail scale.
constexpr WaveComponent Waves[] = {
    {WindX / WindLength, WindZ / WindLength, 48.0f, SeaState * 2.00f, 0.017f},
    {0.570f, 0.822f, 96.0f, SeaState * 1.60f, 0.029f},
    {WindX / WindLength, WindZ / WindLength, 144.0f, SeaState * 2.70f, 0.043f},
    {0.944f, 0.330f, 288.0f, SeaState * 2.10f, 0.071f},
    {WindX / WindLength, WindZ / WindLength, 432.0f, SeaState * 3.30f, 0.113f},
    {0.710f, 0.704f, 1296.0f, SeaState * 3.80f, 0.191f},
};
} // namespace

WaterSurfaceSample QueryWaterSurface(float worldX, float worldZ, float time, float seaLevel)
{
    WaterSurfaceSample sample = {seaLevel, 0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 0.0f};
    float slopeX = 0.0f;
    float slopeZ = 0.0f;

    for (const WaveComponent& wave : Waves)
    {
        const float waveNumber = Pi2 / wave.length;
        // Deep-water dispersion, nudged by the stable renderer wind-speed default.
        const float angularSpeed = std::sqrt(Gravity * waveNumber) * (0.75f + WindSpeed * 0.12f);
        const float phase = waveNumber * (wave.directionX * worldX + wave.directionZ * worldZ) - angularSpeed * time +
                            SpectrumSeed * wave.seedPhase;
        const float sine = std::sin(phase);
        const float cosine = std::cos(phase);
        const float slope = wave.amplitude * waveNumber * cosine;
        const float steepness = 0.08f / (waveNumber * wave.amplitude * 6.0f);

        sample.height += wave.amplitude * sine;
        slopeX += wave.directionX * slope;
        slopeZ += wave.directionZ * slope;
        // The horizontal velocity is the time derivative of a bounded Gerstner drift.
        sample.velocityX += steepness * wave.amplitude * wave.directionX * angularSpeed * sine;
        sample.velocityZ += steepness * wave.amplitude * wave.directionZ * angularSpeed * sine;
    }

    const float normalLength = std::sqrt(slopeX * slopeX + 1.0f + slopeZ * slopeZ);
    sample.normalX = -slopeX / normalLength;
    sample.normalY = 1.0f / normalLength;
    sample.normalZ = -slopeZ / normalLength;
    sample.roughness = std::sqrt(slopeX * slopeX + slopeZ * slopeZ);
    return sample;
}

} // namespace Poseidon
