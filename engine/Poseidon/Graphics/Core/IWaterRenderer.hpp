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

} // namespace Poseidon
