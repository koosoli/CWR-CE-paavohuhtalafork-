#include <Poseidon/Graphics/Core/IWaterRenderer.hpp>

namespace Poseidon
{
namespace
{
// One snapshot, replaced wholesale each water frame. The publisher is the render
// thread and the only reader is the test harness, which reads between frames, so
// a plain object is sufficient here; there is no partial-update window worth
// locking for, and a lock on the water draw path would be a real cost for a
// diagnostic that production never reads.
WaterFrameStats gLastWaterFrameStats;
} // namespace

void PublishWaterFrameStats(const WaterFrameStats& stats)
{
    gLastWaterFrameStats = stats;
    gLastWaterFrameStats.published = true;
}

const WaterFrameStats& LastWaterFrameStats()
{
    return gLastWaterFrameStats;
}

} // namespace Poseidon
