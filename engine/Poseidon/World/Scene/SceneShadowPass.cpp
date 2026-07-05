#include <Poseidon/Core/Config/EngineConfig.hpp>
#include <Poseidon/Core/Global.hpp>

#include <Poseidon/World/Scene/Scene.hpp>
#include <Poseidon/Graphics/Core/Engine.hpp>
#include <Poseidon/Graphics/Rendering/Lighting/Lights.hpp>
#include <Poseidon/World/Scene/Object.hpp>
#include <Poseidon/World/Scene/Camera/Camera.hpp>
#include <Poseidon/World/Simulation/FrameInv.hpp>
#include <Poseidon/Graphics/Rendering/Primitives/Poly.hpp>
#include <Poseidon/Graphics/Shadow/ShadowMath.hpp>

#include <chrono>
#include <cstring>
#include <unordered_map>
#include <vector>
#include <array>
#include <utility>
#include <Poseidon/Foundation/Containers/StreamArray.hpp>
#include <Poseidon/Foundation/Math/Math3D.hpp>
#include <Poseidon/Foundation/Math/Math3DP.hpp>
#include <Poseidon/Foundation/Math/MathOpt.hpp>
#include <Poseidon/Foundation/Types/Pointers.hpp>

// Test/debug instrumentation: count of solid shadow-caster vertices contributed by
// PROXIES (weapon, crew) on the last depth pass. >0 proves the proxy actually cast.
// Read by the triShadowProxyVerts verb.

namespace Poseidon
{
static int g_lastShadowProxyVerts = 0;
} // namespace Poseidon
int LastShadowProxyVertCount()
{
    using namespace Poseidon;
    return g_lastShadowProxyVerts;
}
namespace Poseidon
{

// World-space caster-vert cache for STATIC casters. Re-walking every visible
// caster's faces and re-transforming ~450k verts to camera-relative each frame
// cost 4.1 ms CPU in a battle scene; statics never change pose, so their world
// verts are collected once and emitted with a bulk camera subtract (~10× less
// work). Dynamic casters (units) still collect live each frame.
namespace
{
struct StaticCasterCache
{
    OLink<Object> obj; // nulls on delete; also catches address reuse
    int geomLOD = -1;
    Vector3 pos = VZero; // transform fingerprint — destruction rotates/moves
    Vector3 dir = VZero;
    std::vector<float> solidWorld; // xyz world
    struct AlphaChunk
    {
        Texture* tex;
        int first; // vertex index into alphaWorld (5 floats per vertex)
        int count;
    };
    std::vector<AlphaChunk> alphaChunks;
    std::vector<float> alphaWorld; // xyzuv world
};
std::unordered_map<Object*, StaticCasterCache> s_staticCasterCache;
// Bound the cache footprint: typical shadow range holds a few hundred statics;
// long camera travel accumulates stale entries — drop everything and let the
// next frame rebuild what it actually uses.
constexpr size_t kStaticCasterCacheCap = 1024;
} // namespace

namespace
{
// Cascade fit shared by the CPU-soup and GPU-submission depth passes: fit in
// camera-relative space, snapped to a world grid (BuildShadowCascadesTiered,
// proven by [ShadowMath] unit tests), so the shadow stays world-anchored while
// the casters stay camera-relative.
shadow::CascadeSet BuildCascadeSet(const Camera* cam, Vector3Val sunDir, const Engine::ShadowMapTuning& tune)
{
    namespace sm = Poseidon::shadow;
    Vector3Val camPos = cam->Position();
    Vector3Val cF = cam->Direction();
    Vector3Val cR = cam->DirectionAside();
    Vector3Val cU = cam->DirectionUp();

    sm::CascadeBuildParams bp;
    bp.camPos = {camPos.X(), camPos.Y(), camPos.Z()};
    bp.forward = {cF.X(), cF.Y(), cF.Z()};
    bp.right = {cR.X(), cR.Y(), cR.Z()};
    bp.up = {cU.X(), cU.Y(), cU.Z()};
    bp.tanHalfX = cam->Left(); // tan(hfov/2)
    bp.tanHalfY = cam->Top();  // tan(vfov/2)
    bp.nearD = cam->ClipNear();
    // Pair the cascade reach with the shadow visibility distance (Game-menu Shadows
    // slider, clamped to the object distance), not the camera far-clip: the frustum
    // tiers cover exactly as far as shadows are meant to render.
    bp.farD = ENGINE_CONFIG.shadowsZ;
    bp.sunDir = {sunDir.X(), sunDir.Y(), sunDir.Z()};
    bp.count = tune.cascadeCount;
    bp.distanceCoef = tune.distanceCoef;
    bp.splitCoef = tune.splitCoef;
    bp.resolution = tune.resolution;
    bp.zPad = 50.0f;
    bp.omniCount = tune.omniCount;
    bp.omniCoef[0] = tune.omniCoef0;
    bp.omniCoef[1] = tune.omniCoef1;
    return sm::BuildShadowCascadesTiered(bp);
}
} // namespace

// Shadow-map depth pass — extracted from Scene::DrawObjectsAndShadowsPass2 so
// Scene.cpp stays under the file-size limit.  Collects the visible casters
// (solid + alpha-cutout per texture, camera-relative) plus their proxies, builds
// the tiered omni+frustum cascades, and renders them.  Off by default.
void Scene::RenderShadowMapDepthPass(int nDraw)
{
    namespace sm = Poseidon::shadow;
    if (GEngine->UsesGpuShadowCasters())
    {
        RenderShadowMapDepthPassGpu(nDraw);
        return;
    }
    extern bool gPerfDumpShadowsOnce;
    extern int gSmDepthCachedCasters;
    const auto depthT0 = std::chrono::steady_clock::now();
    int censusCasters = 0, censusRebuilds = 0, censusCached = 0;
    // Solid casters (opaque) → xyz; alpha-cutout casters (foliage) → xyz+uv
    // grouped per texture so the depth pass can alpha-discard on the caster
    // texture and cast a leaf silhouette instead of a solid quad blob.
    static std::vector<float> smVerts;                                    // solid: 3 floats/vert
    static std::vector<float> smAlphaVerts;                               // alpha: 5 floats/vert (xyz+uv)
    static std::vector<Engine::ShadowCasterBatch> smAlphaBatches;         // per-texture ranges
    static std::unordered_map<Texture*, std::vector<float>> s_alphaByTex; // scratch, reused
    smVerts.clear();
    smAlphaVerts.clear();
    smAlphaBatches.clear();
    g_lastShadowProxyVerts = 0;
    for (auto& kv : s_alphaByTex)
    {
        kv.second.clear();
    }
    if (s_staticCasterCache.size() > kStaticCasterCacheCap)
    {
        s_staticCasterCache.clear();
    }
    const int kSkipMask = NoShadow | ShadowDisabled | IsHidden | IsHiddenProxy;
    const int kAlphaMask = IsAlpha | IsTransparent;
    const Vector3 camPos = GetCamera()->Position(); // camera-relative render (PrepareMeshTLImpl)
    // Only collect casters within the shadow-map far distance (= distanceCoef *
    // SHADOW visibility distance); nothing beyond it samples them, so distanceCoef
    // doubles as the caster-collection budget. Pair the reach with
    // ENGINE_CONFIG.shadowsZ — the Game-menu Shadows slider, already clamped to the
    // object distance (no casters live past where objects draw).
    const Engine::ShadowMapTuning smCap = GEngine->GetShadowMapTuning();
    const float smCamNear = GetCamera()->ClipNear();
    const float smShadowsZ = ENGINE_CONFIG.shadowsZ;
    const float smShadowFar = smCamNear + smCap.distanceCoef * (smShadowsZ - smCamNear);
    const float smCasterMaxDist2 = Square(smShadowFar);

    // Walk one shape's faces in WORLD space, dispatching triangles to the emit
    // callbacks. Used for live (camera-relative stream) and cache (world-space
    // store) collection so both share the classification + triangulation.
    auto walkShape = [&](Shape* s, const FrameBase& frame, auto&& emitSolid, auto&& emitAlpha)
    {
        for (Offset o = s->BeginFaces(); o < s->EndFaces(); s->NextFace(o))
        {
            const Poly& f = s->Face(o);
            sm::CasterMode mode = sm::ClassifyShadowCaster(f.Special(), kSkipMask, kAlphaMask);
            if (mode == sm::CasterMode::Skip)
            {
                continue;
            }
            Texture* tex = f.GetTexture();
            if (mode == sm::CasterMode::AlphaTest && !tex)
            {
                mode = sm::CasterMode::Solid; // no texture to test — cast solid
            }
            int fn = f.N();
            for (int k = 1; k + 1 < fn; k++)
            {
                int i0 = f.GetVertex(0), ia = f.GetVertex(k), ib = f.GetVertex(k + 1);
                Vector3 w0 = frame.PositionModelToWorld(s->Pos(i0));
                Vector3 wa = frame.PositionModelToWorld(s->Pos(ia));
                Vector3 wb = frame.PositionModelToWorld(s->Pos(ib));
                if (mode == sm::CasterMode::Solid)
                {
                    emitSolid(w0, wa, wb);
                }
                else
                {
                    emitAlpha(tex, s, w0, i0, wa, ia, wb, ib);
                }
            }
        }
    };

    // Live (uncached) collection — camera-relative into the stream buffers.
    auto liveSolid = [&](const Vector3& w0, const Vector3& wa, const Vector3& wb)
    {
        auto push = [&](const Vector3& w)
        {
            smVerts.push_back(w.X() - camPos.X());
            smVerts.push_back(w.Y() - camPos.Y());
            smVerts.push_back(w.Z() - camPos.Z());
        };
        push(w0);
        push(wa);
        push(wb);
    };
    auto liveAlpha =
        [&](Texture* tex, Shape* s, const Vector3& w0, int i0, const Vector3& wa, int ia, const Vector3& wb, int ib)
    {
        std::vector<float>& buf = s_alphaByTex[tex];
        auto push = [&](const Vector3& w, int vi)
        {
            buf.push_back(w.X() - camPos.X());
            buf.push_back(w.Y() - camPos.Y());
            buf.push_back(w.Z() - camPos.Z());
            buf.push_back(tex->UToLogical(s->U(vi)));
            buf.push_back(tex->VToLogical(s->V(vi)));
        };
        push(w0, i0);
        push(wa, ia);
        push(wb, ib);
    };
    auto collectShape = [&](Shape* s, const FrameBase& frame) { walkShape(s, frame, liveSolid, liveAlpha); };

    // Cached static collection: build the entry's world-space buffers once,
    // emit per frame with a bulk camera-relative subtract.
    auto buildStaticEntry = [&](StaticCasterCache& e, Object* obj, int geomLOD, Shape* s)
    {
        e.obj = obj;
        e.geomLOD = geomLOD;
        e.pos = obj->Position();
        e.dir = obj->Direction();
        e.solidWorld.clear();
        e.alphaChunks.clear();
        e.alphaWorld.clear();
        auto cacheSolid = [&](const Vector3& w0, const Vector3& wa, const Vector3& wb)
        {
            auto push = [&](const Vector3& w)
            {
                e.solidWorld.push_back(w.X());
                e.solidWorld.push_back(w.Y());
                e.solidWorld.push_back(w.Z());
            };
            push(w0);
            push(wa);
            push(wb);
        };
        auto cacheAlpha = [&](Texture* tex, Shape* sh, const Vector3& w0, int i0, const Vector3& wa, int ia,
                              const Vector3& wb, int ib)
        {
            if (e.alphaChunks.empty() || e.alphaChunks.back().tex != tex)
            {
                e.alphaChunks.push_back({tex, static_cast<int>(e.alphaWorld.size() / 5), 0});
            }
            auto push = [&](const Vector3& w, int vi)
            {
                e.alphaWorld.push_back(w.X());
                e.alphaWorld.push_back(w.Y());
                e.alphaWorld.push_back(w.Z());
                e.alphaWorld.push_back(tex->UToLogical(sh->U(vi)));
                e.alphaWorld.push_back(tex->VToLogical(sh->V(vi)));
            };
            push(w0, i0);
            push(wa, ia);
            push(wb, ib);
            e.alphaChunks.back().count += 3;
        };
        walkShape(s, *obj, cacheSolid, cacheAlpha);
    };
    const float camX = camPos.X(), camY = camPos.Y(), camZ = camPos.Z();
    auto emitCachedEntry = [&](const StaticCasterCache& e)
    {
        const size_t base = smVerts.size();
        smVerts.resize(base + e.solidWorld.size());
        const float* src = e.solidWorld.data();
        float* dst = smVerts.data() + base;
        for (size_t i = 0; i < e.solidWorld.size(); i += 3)
        {
            dst[i] = src[i] - camX;
            dst[i + 1] = src[i + 1] - camY;
            dst[i + 2] = src[i + 2] - camZ;
        }
        for (const StaticCasterCache::AlphaChunk& chunk : e.alphaChunks)
        {
            std::vector<float>& buf = s_alphaByTex[chunk.tex];
            const size_t b2 = buf.size();
            buf.resize(b2 + static_cast<size_t>(chunk.count) * 5);
            const float* asrc = e.alphaWorld.data() + static_cast<size_t>(chunk.first) * 5;
            float* adst = buf.data() + b2;
            for (int i = 0; i < chunk.count; i++, asrc += 5, adst += 5)
            {
                adst[0] = asrc[0] - camX;
                adst[1] = asrc[1] - camY;
                adst[2] = asrc[2] - camZ;
                adst[3] = asrc[3];
                adst[4] = asrc[4];
            }
        }
    };

    for (int i = 0; i < nDraw; i++)
    {
        SortObject* oi = _drawMergers[i];
        if (!oi->object || !oi->shape)
        {
            continue;
        }
        // Beyond the shadow range nothing samples this caster's depth.
        if (oi->distance2 > smCasterMaxDist2)
        {
            continue;
        }
        // Cast gate INDEPENDENT of the engine's shadow DISTANCE cull. The engine sets
        // shadowLOD = INVISIBLE beyond _shadowFogMaxRange (a near range tuned for the
        // projected path), which would leave the full-VD frustum tiers with no
        // casters. Shadow maps want every real caster the camera can see, so gate
        // only on "this never casts" (model NoShadow flag / CastShadow opt-out);
        // per-face NoShadow / alpha is still handled by ClassifyShadowCaster below.
        if ((oi->shape->Special() & NoShadow) || !oi->object->CastShadow())
        {
            continue;
        }
        // Visible objects cast from their DRAW LOD (silhouette matches the screen,
        // faithful at any distance); an object that is itself off-screen (drawLOD
        // invisible) but whose shadow falls into view — e.g. someone behind the
        // camera — casts from its shadow LOD so the omni tiers still pick it up.
        int geomLOD = (oi->drawLOD != LOD_INVISIBLE) ? oi->drawLOD : oi->shadowLOD;
        if (geomLOD == LOD_INVISIBLE)
        {
            continue;
        }
        // Caster LOD is re-selected at a biased distance: a depth-map texel covers
        // decimetres, so a caster needs far fewer vertices than its screen draw —
        // 250 battle soldiers at the 744-vert LOD dominated the collection (4 ms
        // CPU/frame). Never select finer than the draw LOD, and keep the screen
        // LOD untouched near the camera where the silhouette is texel-resolved.
        if (smCap.casterLodBias > 1.0f)
        {
            const float biased2 = oi->distance2 * Square(smCap.casterLodBias);
            int coarse = LevelFromDistance2(oi->shape, biased2, oi->object->Scale(), oi->object->Direction(),
                                            GetCamera()->Direction());
            if (coarse != LOD_INVISIBLE && coarse > geomLOD)
            {
                geomLOD = coarse;
            }
        }
        const FrameBase& fb = *oi->object;
        Shape* s = oi->shape->LevelOpaque(geomLOD);
        if (!s || s->NPos() <= 0)
        {
            continue;
        }
        censusCasters++;
        if (oi->object->Static())
        {
            StaticCasterCache& e = s_staticCasterCache[oi->object];
            if (e.obj != oi->object || e.geomLOD != geomLOD || (e.pos - oi->object->Position()).SquareSize() != 0 ||
                (e.dir - oi->object->Direction()).SquareSize() != 0)
            {
                buildStaticEntry(e, oi->object, geomLOD, s);
                censusRebuilds++;
            }
            else
            {
                censusCached++;
            }
            emitCachedEntry(e);
        }
        else
        {
            collectShape(s, fb);
        }

        // Proxies (the soldier's weapon, vehicle crew, ...) cast from their
        // substituted model — the parent holds only an IsHiddenProxy placeholder
        // (skipped above; that placeholder was the "proxy arrow"). Mirror the
        // projected path's proxy loop (Scene.cpp DrawExShadow): gate on
        // CastProxyShadow, compose the proxy world transform, cast its visible LOD.
        Object* obj = oi->object;
        const size_t solidBeforeProxy = smVerts.size();
        const int proxyCount = obj->GetProxyCount(geomLOD);
        for (int pi = 0; pi < proxyCount; pi++)
        {
            if (!obj->CastProxyShadow(geomLOD, pi))
            {
                continue;
            }
            Matrix4 ptrans = obj->Transform(), pinv = obj->GetInvTransform();
            LODShapeWithShadow* pshape = nullptr;
            Object* proxy = obj->GetProxy(pshape, geomLOD, ptrans, pinv, *obj, pi);
            if (!proxy || !pshape)
            {
                continue;
            }
            const float pdist2 = ptrans.Position().Distance2(camPos);
            int plevel =
                LevelFromDistance2(pshape, pdist2, ptrans.Scale(), ptrans.Direction(), GetCamera()->Direction());
            if (plevel == LOD_INVISIBLE)
            {
                continue;
            }
            Shape* ps = pshape->LevelOpaque(plevel);
            if (!ps || ps->NPos() <= 0)
            {
                continue;
            }
            FrameWithInverse pframe(ptrans, pinv);
            collectShape(ps, pframe);
        }
        g_lastShadowProxyVerts += static_cast<int>((smVerts.size() - solidBeforeProxy) / 3);
    }
    gSmDepthCachedCasters = censusCached;
    // Flatten the per-texture alpha buckets into one buffer + batch ranges.
    for (auto& kv : s_alphaByTex)
    {
        if (kv.second.empty())
        {
            continue;
        }
        Engine::ShadowCasterBatch batch;
        batch.texture = kv.first;
        batch.firstVertex = static_cast<int>(smAlphaVerts.size() / 5);
        batch.vertexCount = static_cast<int>(kv.second.size() / 5);
        smAlphaVerts.insert(smAlphaVerts.end(), kv.second.begin(), kv.second.end());
        smAlphaBatches.push_back(batch);
    }
    const Engine::ShadowMapTuning tune = GEngine->GetShadowMapTuning();
    const int smRes = tune.resolution;
    const Camera* cam = GetCamera();
    Vector3Val cF = cam->Direction();
    const sm::CascadeSet cs = BuildCascadeSet(cam, _mainLight->ShadowDirection(), tune);
    if (gPerfDumpShadowsOnce)
    {
        const double collectMs =
            std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - depthT0).count();
        LOG_INFO(Graphics,
                 "PERF smdepth: {} casters ({} cached, {} rebuilt), {} solid + {} alpha verts collected in {:.2f} "
                 "ms CPU",
                 censusCasters, censusCached, censusRebuilds, (int)(smVerts.size() / 3), (int)(smAlphaVerts.size() / 5),
                 collectMs);
        gPerfDumpShadowsOnce = false;
    }

    float vps[4 * 16];
    for (int c = 0; c < cs.count; c++)
    {
        memcpy(vps + c * 16, cs.camRelVP[c].m.data(), sizeof(float) * 16);
    }
    const float camFwd[3] = {cF.X(), cF.Y(), cF.Z()};
    Engine::ShadowCasterSet casterSet;
    casterSet.solidXYZ = smVerts.data();
    casterSet.solidVertexCount = static_cast<int>(smVerts.size() / 3);
    casterSet.alphaXYZUV = smAlphaVerts.data();
    casterSet.alphaVertexCount = static_cast<int>(smAlphaVerts.size() / 5);
    casterSet.alphaBatches = smAlphaBatches.data();
    casterSet.alphaBatchCount = static_cast<int>(smAlphaBatches.size());
    GEngine->RenderShadowDepthScene(vps, cs.splitViewDist, camFwd, cs.count, cs.omniCount, smRes, casterSet);
}

// GPU-driven depth pass: same caster policy as the soup collection above, but
// casters are submitted as mesh + transform.
// RT-animated casters hand their bone palette to the backend and are
// posed in the depth shader.
void Scene::RenderShadowMapDepthPassGpu(int nDraw)
{
    const Engine::ShadowMapTuning tune = GEngine->GetShadowMapTuning();
    const Camera* cam = GetCamera();
    GEngine->SetShadowCascades(BuildCascadeSet(cam, _mainLight->ShadowDirection(), tune), tune.resolution);

    const Vector3 camPos = cam->Position();
    const float camNear = cam->ClipNear();
    const float shadowFar = camNear + tune.distanceCoef * (ENGINE_CONFIG.shadowsZ - camNear);
    const float casterMaxDist2 = Square(shadowFar);

    for (int i = 0; i < nDraw; i++)
    {
        SortObject* oi = _drawMergers[i];
        if (!oi->object || !oi->shape)
        {
            continue;
        }
        if (oi->distance2 > casterMaxDist2)
        {
            continue;
        }
        if ((oi->shape->Special() & NoShadow) || !oi->object->CastShadow())
        {
            continue;
        }
        int geomLOD = (oi->drawLOD != LOD_INVISIBLE) ? oi->drawLOD : oi->shadowLOD;
        if (geomLOD == LOD_INVISIBLE)
        {
            continue;
        }
        if (tune.casterLodBias > 1.0f)
        {
            const float biased2 = oi->distance2 * Square(tune.casterLodBias);
            int coarse = LevelFromDistance2(oi->shape, biased2, oi->object->Scale(), oi->object->Direction(),
                                            cam->Direction());
            if (coarse != LOD_INVISIBLE && coarse > geomLOD)
            {
                geomLOD = coarse;
            }
        }
        Shape* s = oi->shape->LevelOpaque(geomLOD);
        if (!s || s->NPos() <= 0)
        {
            continue;
        }
        if (!s->GetVertexBuffer())
        {
            s->ConvertToVBuffer(s->NVertex() > 1024 ? VBBigDiscardable : VBSmallDiscardable);
            if (!s->GetVertexBuffer())
            {
                continue;
            }
        }
        // Animate routes the pose to the backend: RT-animated casters get their
        // bone palette set on the vertex buffer (AnimationRT::ApplyMatrices GPU
        // path), morphing/surface-conforming casters refresh CPU verts. Statics
        // skip it (Animate is a no-op for them).
        Object* obj = oi->object;
        const bool animate = !obj->Static();
        // Publish a mode-2 conform plane so the depth shader conforms ClipLand vegetation
        // per vertex (mirroring the color pass): Animate then skips the CPU deform and
        // AddShadowCaster uploads OrigPos once, collapsing the per-instance shadow-mesh
        // upload storm. No-op on GL33 / non-ClipLand casters (they keep deforming).
        ConformPlane savedConform;
        const bool publishedConform = obj->PublishConformPlane(s, savedConform);
        if (animate)
        {
            obj->Animate(geomLOD);
        }
        GEngine->AddShadowCaster(*s, obj->Transform());
        if (animate)
        {
            obj->Deanimate(geomLOD);
        }
        if (publishedConform)
        {
            GCurrentConformPlane = savedConform;
        }

        // Proxies (the soldier's weapon, vehicle crew, ...) cast from their
        // substituted model — the parent holds only an IsHiddenProxy placeholder.
        const int proxyCount = obj->GetProxyCount(geomLOD);
        for (int pi = 0; pi < proxyCount; pi++)
        {
            if (!obj->CastProxyShadow(geomLOD, pi))
            {
                continue;
            }
            Matrix4 ptrans = obj->Transform(), pinv = obj->GetInvTransform();
            LODShapeWithShadow* pshape = nullptr;
            Object* proxy = obj->GetProxy(pshape, geomLOD, ptrans, pinv, *obj, pi);
            if (!proxy || !pshape)
            {
                continue;
            }
            const float pdist2 = ptrans.Position().Distance2(camPos);
            int plevel =
                LevelFromDistance2(pshape, pdist2, ptrans.Scale(), ptrans.Direction(), cam->Direction());
            if (plevel == LOD_INVISIBLE)
            {
                continue;
            }
            Shape* ps = pshape->LevelOpaque(plevel);
            if (!ps || ps->NPos() <= 0)
            {
                continue;
            }
            if (!ps->GetVertexBuffer())
            {
                ps->ConvertToVBuffer(ps->NVertex() > 1024 ? VBBigDiscardable : VBSmallDiscardable);
                if (!ps->GetVertexBuffer())
                {
                    continue;
                }
            }
            GEngine->AddShadowCaster(*ps, ptrans);
        }
    }
}

} // namespace Poseidon
