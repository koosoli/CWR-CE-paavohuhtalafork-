#pragma once

#include <cstdint>

namespace Poseidon
{
constexpr uint32_t HydroMaxWaterInteractions = 48;

enum HydroWaterInteractionKind : uint32_t
{
    HydroWaterInteractionBullet = 0,
    HydroWaterInteractionObject = 1,
    HydroWaterInteractionPlayer = 2,
    HydroWaterInteractionExplosion = 3,
    HydroWaterInteractionFootstep = 4,
    HydroWaterInteractionContinuous = 5,
};

enum HydroWaterInteractionFlags : uint32_t
{
    HydroWaterInteractionPendingImpulse = 1u << 0,
    HydroWaterInteractionCapsule = 1u << 8,
    HydroWaterInteractionPlayerWading = 1u << 9,
    HydroWaterInteractionPlayerSwimming = 1u << 10,
    HydroWaterInteractionLeftSide = 1u << 11,
    HydroWaterInteractionLargeBody = 1u << 12,
};

// This mirrors the renderer ABI without making simulation code depend on wgpu_renderer.hpp.
struct alignas(16) HydroWaterInteractionEvent
{
    float positionRadius[4];
    float velocityKind[4];
    float timeLifeFoamMass[4];
    float directionDepthFlags[4];
};

static_assert(sizeof(HydroWaterInteractionEvent) == 64 && alignof(HydroWaterInteractionEvent) == 16,
              "Hydro water event must match the renderer ABI");

// Safe from simulation threads. Events are consumed only by the render path.
void SubmitWaterInteraction(const HydroWaterInteractionEvent& event);
uint32_t DrainWaterInteractions(HydroWaterInteractionEvent* events, uint32_t capacity);

// Player immersion is visual-only. It lets the renderer distinguish a camera above
// water from an infantry body merely standing in shallow water.
void SetPlayerWaterDepth(float depth);
float GetPlayerWaterDepth();

} // namespace Poseidon
