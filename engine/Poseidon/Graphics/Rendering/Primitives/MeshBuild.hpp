#pragma once

#include <Poseidon/Core/Types.hpp>
#include <Poseidon/Graphics/Core/MeshVertex.hpp>
#include <Poseidon/Foundation/Containers/Array.hpp>

namespace Poseidon
{
class Shape;

namespace render::mesh
{

struct MeshSection
{
    int beg, end;             // fan index range [beg, end)
    int begVertex, endVertex; // referenced vertex range [begVertex, endVertex)
};

// Fan-triangulated index count over every face (N-gon -> (N-2)*3).
int CountIndices(const Shape& src);

// `out` must hold src.NVertex() entries.
void BuildVertices(const Shape& src, SVertex* out);

// `out` must hold CountIndices(src) entries.
void BuildIndices(const Shape& src, VertexIndex* out);

void BuildSections(const Shape& src, AutoArray<MeshSection>& out);

} // namespace render::mesh
} // namespace Poseidon
