#include <PoseidonGL33/EngineGL33.hpp>
#include <Poseidon/Graphics/Textures/TexturePreload.hpp>
#include <Poseidon/Graphics/Textures/TextureBank.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Draw2DGeometry.hpp>
#include <Poseidon/World/Scene/Scene.hpp>

void EngineGL33::Draw2D(const Draw2DPars& pars, const Rect2DAbs& rect, const Rect2DAbs& clip)
{
    PoseidonAssert(pars.mip.IsOK());
    if (!pars.mip.IsOK())
    {
        return;
    }

    if (_resetNeeded)
    {
        return;
    }

    // OpenGL uses pixel-corner addressing — vertex positions are taken as
    // pixel coordinates directly (no D3D9 -0.5 shift, no shader half-pixel
    // compensation).
    Draw2DCorners c;
    if (!ClipDraw2DRect(pars, rect, clip, _w, _h, c))
    {
        return;
    }

    TLVertex pos[4];
    pos[0].rhw = 1;
    pos[0].color = pars.colorTL;
    pos[0].specular = PackedColor(0xff000000);
    pos[0].pos[2] = 0.5;

    pos[1].rhw = 1;
    pos[1].color = pars.colorTR;
    pos[1].specular = PackedColor(0xff000000);
    pos[1].pos[2] = 0.5;

    pos[2].rhw = 1;
    pos[2].color = pars.colorBR;
    pos[2].specular = PackedColor(0xff000000);
    pos[2].pos[2] = 0.5;

    pos[3].rhw = 1;
    pos[3].color = pars.colorBL;
    pos[3].specular = PackedColor(0xff000000);
    pos[3].pos[2] = 0.5;

    pos[0].pos[0] = c.xBeg, pos[0].pos[1] = c.yBeg;
    pos[1].pos[0] = c.xEnd, pos[1].pos[1] = c.yBeg;
    pos[2].pos[0] = c.xEnd, pos[2].pos[1] = c.yEnd;
    pos[3].pos[0] = c.xBeg, pos[3].pos[1] = c.yEnd;
    pos[0].t0.u = c.uTL, pos[0].t0.v = c.vTL;
    pos[1].t0.u = c.uTR, pos[1].t0.v = c.vTR;
    pos[2].t0.u = c.uBR, pos[2].t0.v = c.vBR;
    pos[3].t0.u = c.uBL, pos[3].t0.v = c.vBL;

    SwitchRenderMode(RM2DTris);
    BeginScreenPass();

    AddVertices(pos, 4); // note: may flush all queues

    QueuePrepareTriangle(pars.mip, pars.spec);

    Queue2DPoly(pos, 4);
}

void EngineGL33::DrawLine(const Line2DAbs& line, PackedColor c0, PackedColor c1, const Rect2DAbs& clip)
{
    Texture* tex = GPreloadedTextures.New(TextureLine);
    const MipInfo& mip = TextBank()->UseMipmap(tex, 1, 1);

    const int specFlags = NoZBuf | IsAlpha | ClampU | ClampV | IsAlphaFog;

    Vertex2DAbs vertices[4];
    LineToQuad(line, c0, c1, vertices);

    DrawPoly(mip, vertices, 4, clip, specFlags);
}

void EngineGL33::DrawPoly(const MipInfo& mip, const Vertex2DPixel* vertices, int n, const Rect2DPixel& clipRect,
                          int specFlags)
{
    if (_resetNeeded)
    {
        return;
    }

    const int maxN = 32;

    Vertex2DPixel scratch1[maxN];
    Vertex2DPixel scratch2[maxN];
    vertices = ClipPoly2D(vertices, n, clipRect, scratch1, scratch2);
    if (!vertices)
    {
        return;
    }

    if (n > maxN)
    {
        n = maxN;
        Fail("Poly: Too much vertices");
    }

    TLVertex gv[maxN];

    float x2d = Left2D();
    float y2d = Top2D();

    for (int i = 0; i < n; i++)
    {
        TLVertex* v = &gv[i];
        const Vertex2DPixel& vs = vertices[i];

        v->pos[0] = vs.x + x2d;
        v->pos[1] = vs.y + y2d;
        v->pos[2] = vs.z;
        v->rhw = vs.w;
        v->color = vs.color;
        v->specular = PackedColor(0xff000000);
        v->t0.u = vs.u;
        v->t0.v = vs.v;
    }

    SwitchRenderMode(RM2DTris);
    BeginScreenPass();
    AddVertices(gv, n); // note: may flush all queues
    QueuePrepareTriangle(mip, specFlags);
    Queue2DPoly(gv, n);
}

void EngineGL33::DrawPoly(const MipInfo& mip, const Vertex2DAbs* vertices, int n, const Rect2DAbs& clipRect,
                          int specFlags)
{
    if (_resetNeeded)
    {
        return;
    }

    const int maxN = 32;

    Vertex2DAbs scratch1[maxN];
    Vertex2DAbs scratch2[maxN];
    vertices = ClipPoly2D(vertices, n, clipRect, scratch1, scratch2);
    if (!vertices)
    {
        return;
    }

    if (n > maxN)
    {
        n = maxN;
        Fail("Poly: Too much vertices");
    }

    TLVertex gv[maxN];

    for (int i = 0; i < n; i++)
    {
        TLVertex* v = &gv[i];
        const Vertex2DAbs& vs = vertices[i];

        v->pos[0] = vs.x;
        v->pos[1] = vs.y;
        v->pos[2] = vs.z;
        v->rhw = vs.w;
        v->color = vs.color;
        v->specular = PackedColor(0xff000000);
        v->t0.u = vs.u;
        v->t0.v = vs.v;
    }

    SwitchRenderMode(RM2DTris);
    BeginScreenPass();
    AddVertices(gv, n); // note: may flush all queues
    QueuePrepareTriangle(mip, specFlags);
    Queue2DPoly(gv, n);
}

void EngineGL33::DrawLine(int beg, int end)
{
    const TLVertex& v0 = _mesh->GetVertex(beg);
    const TLVertex& v1 = _mesh->GetVertex(end);

    float x0 = v0.pos.X();
    float y0 = v0.pos.Y();
    float x1 = v1.pos.X();
    float y1 = v1.pos.Y();

    float z0 = v0.pos.Z();
    float z1 = v1.pos.Z();
    float w0 = v0.rhw;
    float w1 = v1.rhw;

    // use line texture
    Texture* tex = GPreloadedTextures.New(TextureLine);
    const MipInfo& mip = TextBank()->UseMipmap(tex, 1, 1);

    // convert line to poly;
    int specFlags = NoZWrite | IsAlpha | ClampU | ClampV | IsAlphaFog;
    float dx = x1 - x0;
    float dy = y1 - y0;
    float dSize2 = dx * dx + dy * dy;
    float invDSize = dSize2 > 0 ? InvSqrt(dSize2) : 1;

    float dSize = dSize2 * invDSize;

    // direction perpendicular dx, dy
    // 2D line drawing
    float pdx = +dy * invDSize, pdy = -dx * invDSize;
    float w = 3.0f;
    x0 -= pdx * (w * 0.5);
    x1 -= pdx * (w * 0.5);
    y0 -= pdy * (w * 0.5);
    y1 -= pdy * (w * 0.5);
    float x0Side = x0 + pdx * w, y0Side = y0 + pdy * w;
    float x1Side = x1 + pdx * w, y1Side = y1 + pdy * w;

    Vertex2DAbs vertices[4];
    float off = 0.0f;
    vertices[0].x = x0 - off;
    vertices[0].y = y0 - off;
    vertices[0].z = z0;
    vertices[0].w = w0;
    vertices[0].u = 0;
    vertices[0].v = 0.25;
    vertices[0].color = v0.color;

    vertices[1].x = x0Side - off;
    vertices[1].y = y0Side - off;
    vertices[1].z = z0;
    vertices[1].w = w0;
    vertices[1].u = 0;
    vertices[1].v = 1;
    vertices[1].color = v0.color;

    vertices[2].x = x1Side - off;
    vertices[2].y = y1Side - off;
    vertices[2].z = z1;
    vertices[2].w = w1;
    vertices[2].u = dSize;
    vertices[2].v = 1;
    vertices[2].color = v1.color;

    vertices[3].x = x1 - off;
    vertices[3].y = y1 - off;
    vertices[3].z = z1;
    vertices[3].w = w1;
    vertices[3].u = dSize;
    vertices[3].v = 0.25;
    vertices[3].color = v1.color;

    Rect2DAbs clip(0, 0, _w, _h);

    DrawPoly(mip, vertices, 4, clip, specFlags);
}
