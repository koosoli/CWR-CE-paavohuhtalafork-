#include <Poseidon/Graphics/Rendering/WaterInteractionBridge.hpp>

#include <array>
#include <mutex>

namespace Poseidon
{
namespace
{
std::array<HydroWaterInteractionEvent, HydroMaxWaterInteractions> pendingEvents;
uint32_t pendingCount = 0;
std::mutex pendingEventsMutex;
} // namespace

void SubmitWaterInteraction(const HydroWaterInteractionEvent& event)
{
    std::lock_guard<std::mutex> lock(pendingEventsMutex);
    if (pendingCount == HydroMaxWaterInteractions)
    {
        // Preserve the newest visual evidence when a simulation frame overproduces.
        for (uint32_t i = 1; i < pendingCount; ++i)
        {
            pendingEvents[i - 1] = pendingEvents[i];
        }
        --pendingCount;
    }
    pendingEvents[pendingCount++] = event;
}

uint32_t DrainWaterInteractions(HydroWaterInteractionEvent* events, uint32_t capacity)
{
    if (events == nullptr || capacity == 0)
    {
        return 0;
    }

    std::lock_guard<std::mutex> lock(pendingEventsMutex);
    const uint32_t count = pendingCount < capacity ? pendingCount : capacity;
    for (uint32_t i = 0; i < count; ++i)
    {
        events[i] = pendingEvents[i];
    }
    for (uint32_t i = count; i < pendingCount; ++i)
    {
        pendingEvents[i - count] = pendingEvents[i];
    }
    pendingCount -= count;
    return count;
}

} // namespace Poseidon
