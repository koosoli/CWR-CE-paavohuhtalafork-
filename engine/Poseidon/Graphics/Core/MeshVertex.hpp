#pragma once

#include <Poseidon/Foundation/Math/Math3DP.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Vertex.hpp>

namespace Poseidon
{

struct SVertex
{
    Vector3P pos;
    // Normals are negated (matches the D3D convention).
    Vector3P norm;
    UVPair t0;
};

} // namespace Poseidon
