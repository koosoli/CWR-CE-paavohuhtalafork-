#include <Poseidon/Graphics/Rendering/Primitives/MeshBuild.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Poly.hpp>
#include <Poseidon/Graphics/Rendering/Shape/Shape.hpp>

#include <climits>

namespace Poseidon::render::mesh
{

int CountIndices(const Shape& src)
{
    int indices = 0;
    for (Offset o = src.BeginFaces(); o < src.EndFaces(); src.NextFace(o))
    {
        const Poly& poly = src.Face(o);
        PoseidonAssert(poly.N() >= 3);
        indices += (poly.N() - 2) * 3;
    }
    return indices;
}

void BuildVertices(const Shape& src, SVertex* out)
{
    const UVPair* uv = &src.UV(0);
    const Vector3* pos = &src.Pos(0);
    const Vector3* norm = &src.Norm(0);
    for (int i = src.NVertex(); --i >= 0;)
    {
        out->pos = Vector3P(pos->X(), pos->Y(), pos->Z());
        // Normals are negated (matches the D3D convention).
        out->norm = Vector3P(-norm->X(), -norm->Y(), -norm->Z());
        out->t0 = *uv;
        pos++;
        norm++;
        uv++;
        out++;
    }
}

void BuildIndices(const Shape& src, VertexIndex* out)
{
    for (Offset o = src.BeginFaces(); o < src.EndFaces(); src.NextFace(o))
    {
        const Poly& poly = src.Face(o);
        for (int i = 2; i < poly.N(); i++)
        {
            *out++ = poly.GetVertex(0);
            *out++ = poly.GetVertex(i - 1);
            *out++ = poly.GetVertex(i);
        }
    }
}

void BuildSections(const Shape& src, AutoArray<MeshSection>& out)
{
    out.Realloc(src.NSections());
    out.Resize(src.NSections());
    int start = 0;
    for (int i = 0; i < src.NSections(); i++)
    {
        const ShapeSection& sec = src.GetSection(i);
        int size = 0;
        int minV = INT_MAX;
        int maxV = 0;
        for (Offset o = sec.beg; o < sec.end; src.NextFace(o))
        {
            const Poly& face = src.Face(o);
            PoseidonAssert(face.N() >= 3);
            size += (face.N() - 2) * 3;
            for (int vv = 0; vv < face.N(); vv++)
            {
                int vi = face.GetVertex(vv);
                if (vi < minV)
                    minV = vi;
                if (vi > maxV)
                    maxV = vi;
            }
        }
        out[i].beg = start;
        out[i].end = start + size;
        out[i].begVertex = minV;
        out[i].endVertex = maxV + 1;
        start += size;
    }
}

} // namespace Poseidon::render::mesh
