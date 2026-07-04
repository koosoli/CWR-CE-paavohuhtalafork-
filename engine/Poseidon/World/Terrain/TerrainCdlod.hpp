// Terrain CDLOD quadtree selection — pure, no engine dependencies.
#pragma once

#include <vector>

namespace Poseidon
{
struct CdlodNode
{
    float originX, originZ;
    float size;
    float minY, maxY;
    int level;
    int child[4]; // child[0] < 0 marks a leaf
};

struct CdlodSelection
{
    float originX, originZ;
    float size;
    int level;
    float morphStart, morphEnd;
};

// ranges[L] is the distance out to which level L is the coarsest acceptable detail.
inline void ComputeCdlodRanges(float baseRange, float ratio, int numLevels, std::vector<float>& ranges)
{
    ranges.resize(numLevels);
    float r = baseRange;
    for (int i = 0; i < numLevels; i++)
    {
        ranges[i] = r;
        r *= ratio;
    }
}

inline float CdlodNodeDistanceSq(const CdlodNode& n, float px, float py, float pz)
{
    const float maxX = n.originX + n.size;
    const float maxZ = n.originZ + n.size;
    const float dx = px < n.originX ? n.originX - px : (px > maxX ? px - maxX : 0.0f);
    const float dy = py < n.minY ? n.minY - py : (py > n.maxY ? py - n.maxY : 0.0f);
    const float dz = pz < n.originZ ? n.originZ - pz : (pz > maxZ ? pz - maxZ : 0.0f);
    return dx * dx + dy * dy + dz * dz;
}

// Distances over which a patch at `level` morphs toward its parent grid; fully
// morphed at ranges[level].
inline void CdlodMorphBand(const std::vector<float>& ranges, int level, float morphRegion, float& morphStart,
                           float& morphEnd)
{
    const float end = ranges[level];
    const float prev = level > 0 ? ranges[level - 1] : 0.0f;
    morphStart = end - (end - prev) * morphRegion;
    morphEnd = end;
}

template <typename EmitFn>
inline void EmitCdlodNode(const CdlodNode& n, int level, const std::vector<float>& ranges, float morphRegion,
                          EmitFn&& emit)
{
    float ms = 0.0f, me = 0.0f;
    CdlodMorphBand(ranges, level, morphRegion, ms, me);
    emit(CdlodSelection{n.originX, n.originZ, n.size, level, ms, me});
}

// Descends the quadtree, choosing each node's level by distance and frustum.
// `visible` frustum-tests a node; `emit` receives each node to draw. Returns
// false when the node is beyond ranges[lodLevel], so the caller draws the area
// coarser instead.
template <typename VisibleFn, typename EmitFn>
bool SelectCdlod(const std::vector<CdlodNode>& nodes, int idx, int lodLevel, float camX, float camY, float camZ,
                 const std::vector<float>& ranges, float morphRegion, VisibleFn&& visible, EmitFn&& emit)
{
    const CdlodNode& n = nodes[idx];
    const float distSq = CdlodNodeDistanceSq(n, camX, camY, camZ);
    if (distSq > ranges[lodLevel] * ranges[lodLevel])
    {
        return false;
    }
    if (!visible(n))
    {
        return true;
    }
    if (lodLevel == 0 || n.child[0] < 0)
    {
        EmitCdlodNode(n, lodLevel, ranges, morphRegion, emit);
        return true;
    }
    if (distSq > ranges[lodLevel - 1] * ranges[lodLevel - 1])
    {
        EmitCdlodNode(n, lodLevel, ranges, morphRegion, emit);
        return true;
    }
    for (int c = 0; c < 4; c++)
    {
        const int childIdx = n.child[c];
        if (!SelectCdlod(nodes, childIdx, lodLevel - 1, camX, camY, camZ, ranges, morphRegion, visible, emit) &&
            visible(nodes[childIdx]))
        {
            // At the child's own level, not the parent's: its morph band must match
            // its geometry level or it draws un-morphed and cracks at coarse edges.
            EmitCdlodNode(nodes[childIdx], lodLevel - 1, ranges, morphRegion, emit);
        }
    }
    return true;
}

} // namespace Poseidon
