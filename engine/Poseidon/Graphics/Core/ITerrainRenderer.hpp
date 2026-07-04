#pragma once

namespace Poseidon
{
class Scene;

class ITerrainRenderer
{
  public:
    virtual ~ITerrainRenderer() = default;

    // Emit the terrain covering land-cell rectangle [xBeg,xEnd) x [zBeg,zEnd) for
    // this frame. Called once per frame from Landscape::DrawGround (opaque layer).
    virtual void DrawTerrain(Scene& scene, int xBeg, int zBeg, int xEnd, int zEnd) = 0;
};

} // namespace Poseidon
