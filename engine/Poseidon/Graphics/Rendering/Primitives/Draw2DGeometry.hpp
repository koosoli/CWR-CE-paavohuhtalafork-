#pragma once

#include <Poseidon/Core/Types.hpp>
#include <Poseidon/Graphics/Core/Engine.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Clip2D.hpp>
#include <Poseidon/Foundation/Common/FltOpts.hpp>
#include <Poseidon/Foundation/Math/MathOpt.hpp>

#include <utility>

namespace Poseidon
{

template <class Vertex, class Rect>
const Vertex* ClipPoly2D(const Vertex* vertices, int& n, const Rect& clipRect, Vertex* scratch1, Vertex* scratch2)
{
    ClipFlags orClip = 0;
    ClipFlags andClip = ClipAll;
    for (int i = 0; i < n; i++)
    {
        const Vertex& vs = vertices[i];
        ClipFlags clip = 0;
        if (vs.x < clipRect.x)
            clip |= ClipLeft;
        else if (vs.x > clipRect.x + clipRect.w)
            clip |= ClipRight;
        if (vs.y < clipRect.y)
            clip |= ClipTop;
        else if (vs.y > clipRect.y + clipRect.h)
            clip |= ClipBottom;
        orClip |= clip;
        andClip &= clip;
    }
    if (andClip)
    {
        n = 0;
        return nullptr;
    }
    if (!orClip)
    {
        return vertices;
    }

    const auto insideLeft = [](const Vertex& v, const Rect& r) { return v.x - r.x; };
    const auto insideRight = [](const Vertex& v, const Rect& r) { return r.x + r.w - v.x; };
    const auto insideTop = [](const Vertex& v, const Rect& r) { return v.y - r.y; };
    const auto insideBottom = [](const Vertex& v, const Rect& r) { return r.y + r.h - v.y; };

    Vertex* freeBuf = scratch1;
    Vertex* used = scratch2;
    for (int i = 0; i < n; i++)
        used[i] = vertices[i];

    if (orClip & ClipTop)
        n = Clip2D(clipRect, freeBuf, used, n, insideTop), std::swap(freeBuf, used);
    if (orClip & ClipBottom)
        n = Clip2D(clipRect, freeBuf, used, n, insideBottom), std::swap(freeBuf, used);
    if (orClip & ClipLeft)
        n = Clip2D(clipRect, freeBuf, used, n, insideLeft), std::swap(freeBuf, used);
    if (orClip & ClipRight)
        n = Clip2D(clipRect, freeBuf, used, n, insideRight), std::swap(freeBuf, used);

    if (n < 3)
    {
        n = 0;
        return nullptr;
    }
    return used;
}

inline void LineToQuad(const Line2DAbs& line, PackedColor c0, PackedColor c1, Vertex2DAbs out[4])
{
    float x0 = line.beg.x, y0 = line.beg.y, x1 = line.end.x, y1 = line.end.y;

    const float dx = x1 - x0, dy = y1 - y0;
    const float dSize2 = dx * dx + dy * dy;
    const float invDSize = dSize2 > 0 ? InvSqrt(dSize2) : 1;

    const float pdx = +dy * invDSize, pdy = -dx * invDSize;
    const float w = 3.0f;
    x0 -= pdx * (w * 0.5f);
    x1 -= pdx * (w * 0.5f);
    y0 -= pdy * (w * 0.5f);
    y1 -= pdy * (w * 0.5f);
    const float x0Side = x0 + pdx * w, y0Side = y0 + pdy * w;
    const float x1Side = x1 + pdx * w, y1Side = y1 + pdy * w;

    out[0].x = x0, out[0].y = y0, out[0].u = 0, out[0].v = 0.25f, out[0].color = c0;
    out[1].x = x0Side, out[1].y = y0Side, out[1].u = 0, out[1].v = 1, out[1].color = c0;
    out[2].x = x1Side, out[2].y = y1Side, out[2].u = 0.1f, out[2].v = 1, out[2].color = c1;
    out[3].x = x1, out[3].y = y1, out[3].u = 0.1f, out[3].v = 0.25f, out[3].color = c1;
}

struct Draw2DCorners
{
    float xBeg, yBeg, xEnd, yEnd;
    float uTL, vTL, uTR, vTR;
    float uBL, vBL, uBR, vBR;
};

inline bool ClipDraw2DRect(const Draw2DPars& pars, const Rect2DAbs& rect, const Rect2DAbs& clip, float w, float h,
                           Draw2DCorners& out)
{
    float xBeg = rect.x, xEnd = xBeg + rect.w;
    float yBeg = rect.y, yEnd = yBeg + rect.h;

    float uBeg = 0, vBeg = 0, uEnd = 1, vEnd = 1;

    const float xc = floatMax(clip.x, 0.0f);
    const float yc = floatMax(clip.y, 0.0f);
    const float xec = floatMin(clip.x + clip.w, w);
    const float yec = floatMin(clip.y + clip.h, h);

    if (xBeg < xc)
        uBeg = (xc - xBeg) / rect.w, xBeg = xc;
    if (xEnd > xec)
        uEnd = 1 - (xEnd - xec) / rect.w, xEnd = xec;
    if (yBeg < yc)
        vBeg = (yc - yBeg) / rect.h, yBeg = yc;
    if (yEnd > yec)
        vEnd = 1 - (yEnd - yec) / rect.h, yEnd = yec;

    if (xBeg >= xEnd || yBeg >= yEnd)
        return false;

    out.xBeg = xBeg, out.yBeg = yBeg, out.xEnd = xEnd, out.yEnd = yEnd;

    out.uTL = pars.uTL + uBeg * (pars.uTR - pars.uTL) + vBeg * (pars.uBL - pars.uTL);
    out.uTR = pars.uTL + uEnd * (pars.uTR - pars.uTL) + vBeg * (pars.uBL - pars.uTL);
    out.uBL = pars.uTL + uBeg * (pars.uTR - pars.uTL) + vEnd * (pars.uBL - pars.uTL);
    out.uBR = pars.uTL + uEnd * (pars.uTR - pars.uTL) + vEnd * (pars.uBL - pars.uTL);

    out.vTL = pars.vTL + uBeg * (pars.vTR - pars.vTL) + vBeg * (pars.vBL - pars.vTL);
    out.vTR = pars.vTL + uEnd * (pars.vTR - pars.vTL) + vBeg * (pars.vBL - pars.vTL);
    out.vBL = pars.vTL + uBeg * (pars.vTR - pars.vTL) + vEnd * (pars.vBL - pars.vTL);
    out.vBR = pars.vTL + uEnd * (pars.vTR - pars.vTL) + vEnd * (pars.vBL - pars.vTL);
    return true;
}

} // namespace Poseidon
