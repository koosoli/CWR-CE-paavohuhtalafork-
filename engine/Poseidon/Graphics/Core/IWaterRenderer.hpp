#pragma once

namespace Poseidon
{
class Scene;

class IWaterRenderer
{
  public:
    virtual ~IWaterRenderer() = default;

    // Emit the water surface covering land-cell rectangle [xBeg,xEnd) x [zBeg,zEnd)
    // for this frame. Called once per frame from Landscape::DrawWater, after the
    // opaque terrain + 3D, so it composites over them and is depth-cut by coastlines.
    virtual void DrawWater(Scene& scene, int xBeg, int zBeg, int xEnd, int zEnd) = 0;
};

// Backend-neutral snapshot of the most recently rendered water frame.
//
// Water currently has no test observability at all: a frozen simulation, a
// collapsed LOD selection, or water that stopped drawing entirely are invisible
// to every existing harness command, which is what blocks TEST-WTR-001 and
// WTR-001's lifecycle testing. The renderer backend publishes this once per
// water frame and the harness reads it back through `triWaterStats`.
//
// Deliberately plain values rather than a handle to the backend: this header
// sits in the graphics interface layer, and the harness in engine/Poseidon must
// not link a concrete graphics backend to observe water. A backend that does not
// implement water (GL33) simply never publishes, and the harness reports that.
struct WaterFrameStats
{
    static constexpr int kLodBuckets = 10;

    unsigned long long frame = 0; // engine frame counter when published
    unsigned int nodes = 0;       // CDLOD nodes selected for this frame
    unsigned int triangles = 0;
    unsigned int lod[kLodBuckets] = {}; // selected node count per LOD level
    bool published = false;
};

// Called by the water backend once per rendered water frame.
void PublishWaterFrameStats(const WaterFrameStats& stats);

// Last published snapshot. `published` is false until a backend publishes one,
// which is itself the signal that no water has been rendered.
const WaterFrameStats& LastWaterFrameStats();

} // namespace Poseidon
