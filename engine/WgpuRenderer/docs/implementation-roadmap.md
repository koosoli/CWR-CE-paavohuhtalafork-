# Renderer implementation roadmap (cross-plan sequencing + dependencies)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** LIVING DOC (2026-07-08). The single source of truth for **what order** the wgpu-renderer
feature plans land in and **what depends on what**. Each plan owns its own design; this doc owns the
edges between them.

> Scope note: this is the *forward-looking feature* roadmap (water, GPU-driven rendering, reflections,
> Forward+). The draw-call/upload-overhead work has its own umbrella,
> [rendering-performance-plan.md](rendering-performance-plan.md); the two meet at the GPU-driven /
> indirect stages (Phase 4). Already-landed or in-flight look work (HDR
> [hdr-pipeline-plan.md](hdr-pipeline-plan.md), procedural sky [procedural-sky-plan.md](procedural-sky-plan.md),
> terrain conform [terrain-conform-vegetation-roads-plan.md](terrain-conform-vegetation-roads-plan.md))
> is upstream context, not sequenced here.

---

## 1. The dependency graph

```
  ┌────────────────────────────────────────────────────────────────────────┐
  │ PHASE 1  Water geometry (GPU CDLOD) + shared CDLOD util   [no deps]      │
  │   water-cdlod-geometry-plan.md                                           │
  │   (optional pull-forward: Gerstner waves — no infra dep)                 │
  └───────────────┬────────────────────────────────────────────────────────┘
                  │  (independent)
  ┌───────────────▼────────────────────────────────────────────────────────┐
  │ PHASE 2  DEPTH PREPASS  ← the keystone (own plan)                        │
  │   depth-prepass-plan.md  (owns it; standalone early-Z win today)         │
  │      ├─ enables ─────────────► Phase 3 (water depth features)            │
  │      ├─ enables ─────────────► Hi-Z / occlusion (Phase 4)               │
  │      └─ used by ─────────────► Forward+ clustered lighting               │
  │   forward-plus-plan.md  = PARALLEL track (clustered lighting), NOT a     │
  │      water blocker; needs only the prepass, not vice-versa               │
  │   ── both designed MSAA-ready (forward, not deferred; A2C foliage) ──    │
  └───────────────┬────────────────────────────────────────────────────────┘
                  │  prepass only (clustered lighting optional/parallel)
  ┌───────────────▼────────────────────────────────────────────────────────┐
  │ PHASE 3  Water depth-dependent rendering                                 │
  │   water-rendering-plan.md  Stages 1–3, 4a, 5                             │
  │   • transparency + depth colour/clarity  (consumes prepass depth;        │
  │       swappable self-provided fallback if prepass slips)                 │
  │   • refraction            (needs a scene-COLOUR copy — independent task)  │
  │   • sky-only reflection 4a (needs sky.wgsl → importable module)          │
  │   • per-map look + Water ImGui tab (sky-coupled)                         │
  └───────────────┬────────────────────────────────────────────────────────┘
                  │
  ┌───────────────▼────────────────────────────────────────────────────────┐
  │ PHASE 4  GPU-driven object rendering + culling  (MULTI-VIEW first-class) │
  │   gpu-object-rendering-plan.md Stage 3 (frustum/distance cull + indirect) │
  │   gpu-culling-and-depth-plan.md (Hi-Z + occlusion, consumes prepass)     │
  │   → migrate terrain+water CDLOD selection to Rust/GPU here               │
  │   → cull pass takes an ARBITRARY camera + optional clip plane            │
  └───────────────┬────────────────────────────────────────────────────────┘
                  │  multi-view cull path
  ┌───────────────▼────────────────────────────────────────────────────────┐
  │ PHASE 5  Full planar scene reflection                                    │
  │   water-rendering-plan.md Stage 4b                                        │
  │   • mirrored camera = "just another view" through the Phase-4 cull path  │
  │   • water surface added as an instance-granularity clip plane            │
  └──────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Phased order (with the decoupling that matters)

### Phase 1 — Water geometry on the GPU + shared CDLOD util
- Land [water-cdlod-geometry-plan.md](water-cdlod-geometry-plan.md): a flat CDLOD water surface
  replacing legacy `_wTable`, gameplay untouched. **No dependency** on prepass/Forward+/culling — ships
  whenever.
- **Extract the shared CDLOD driver** (`BuildCdlodTree` + `SelectVisibleCdlod`, parameterized by
  emit/bounds functors) so terrain and water share it instead of `WaterWgpu` copy-pasting
  `TerrainWgpu`. The pure algorithm ([TerrainCdlod.hpp](../../Poseidon/World/Terrain/TerrainCdlod.hpp))
  is already generic; only the build + per-frame driver need factoring. Keep it **C++ for now** — the
  shared util is what makes the Phase-4 move to Rust/GPU a one-site change.
- **Optional pull-forward:** Gerstner waves + sun specular (water-rendering Stage 1) have **zero** infra
  dependency (pure VS displacement + shading). Doing them here gives a genuinely nice interim ocean.
  Prefer **"vastly simplified but *waved*"** over porting the legacy animated-normal-map texture — that
  texture is throwaway work.

### Phase 2 — Depth prepass (the keystone) + Forward+ (parallel)
- Land the opaque **depth + normal prepass** ([depth-prepass-plan.md](depth-prepass-plan.md)) — now its
  own plan, and **unconditional (no flag)**: it's a hard prerequisite, not a toggle, and on this
  25-year-old low-poly content the extra vertex pass is negligible (the freed budget buys the luxuries —
  planar reflections, high-quality shadows, long draw distance, SSAO). Standalone value today (early-Z
  overdraw reduction for the per-pixel object + terrain shaders); consumers: water (sampleable opaque
  depth), occlusion culling (Hi-Z), Forward+ (overdraw reduction), **SSAO (view-space normal G-buffer,
  written from the start)**.
- **Critical decoupling:** water rendering needs only the **prepass**, *not* the Forward+ clustered
  light-culling. Do not chain water to the full Forward+ effort. Land the prepass early as standalone
  infra; run [forward-plus-plan.md](forward-plus-plan.md) (clustered lighting) and water rendering as
  **independent tracks** afterward. Clustered lighting only *enhances* water later (efficient many-light
  shading on the surface — night harbours).
- **Design for multi-view now:** make the prepass (and any froxel/Hi-Z structures) take an **arbitrary
  camera**, so Phase 5 can build a reflected-view prepass / reflected froxel for aerial fog rather than
  hard-wiring the main camera.
- **Design MSAA-ready now (principle 8):** the renderer stays **forward** specifically to keep MSAA +
  alpha-to-coverage reachable. Make `sample_count` a pipeline parameter across prepass/colour/shading;
  the prepass's sampleable depth becomes a `min`-resolve under MSAA (prepass plan §5); Forward+ shades
  per-pixel (forward-plus §6). None of it is built yet, but nothing here may foreclose it.

### Phase 3 — Water depth-dependent rendering
- Land [water-rendering-plan.md](water-rendering-plan.md) Stages 1–3, 4a, 5: transparency, depth-based
  colour + Beer-Lambert clarity, screen-space refraction, **sky-only** reflection, per-map look + Water
  ImGui tab (sky-coupled).
- **Depth source is swappable.** Water's depth access is written as "sample an opaque-depth texture";
  it consumes the Phase-2 prepass if present, else self-provides (a small `TEXTURE_BINDING` +
  depth-aspect view). So Phase 3 is not *hard*-blocked on Phase 2 — scheduling insurance.
- **Refraction** needs a **pre-water scene-colour copy** (a blit), independent of the prepass.
- **Sky-only reflection (4a)** needs `sky.wgsl` refactored into an importable module exposing
  `sky_radiance(dir)` — no culling, no multi-view. This is why *some* reflection arrives in Phase 3.

### Phase 4 — GPU-driven object rendering + culling (multi-view first-class)
- Land [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) — now the concrete, end-to-end
  spec: a **GPU-resident retained scene** (unified instance buffer, merged geometry pool, bindless object
  textures, delta/patch updates, eager destroyed variants), a compute pass doing **distance + frustum +
  occlusion culling *and* LOD selection**, compacted into **indirect draws**, plus Hi-Z occlusion from
  the Phase-2 prepass. [gpu-object-rendering-plan.md](gpu-object-rendering-plan.md) contributes the
  per-instance record (§5) + destruction morph (Stage 1); its Stage 3 delegates to the culling plan.
- **Portable indirect, Metal must work.** `multi_draw_indexed_indirect` where the feature exists; a
  portable CPU loop of single `draw_indexed_indirect` otherwise (Metal) — runtime-gated, no backend-
  specific code, modestly more draw calls accepted. CPU `plan_3d` path stays as the ultimate fallback.
- **Build the cull/indirect path around an arbitrary camera + optional clip plane** from the start — the
  same capability serves the main view, the depth prepass, shadow cascades, and (Phase 5) planar
  reflections. Frustum + distance + LOD reuse per-view trivially; occlusion does **not** (needs that
  view's own Hi-Z) — fine, shadows/reflections use frustum+distance+LOD.
- **Migrate the terrain + water CDLOD selection to Rust/GPU here.** The Phase-1 shared util localizes
  this to one place; reconstruct frustum planes from `frame.proj * frame.view` (verify parity with
  `Camera::IsClipped`).

### Phase 5 — Full planar scene reflection
- Land [water-rendering-plan.md](water-rendering-plan.md) Stage 4b: a mirrored-camera scene re-render.
- **This is why Phase 4 precedes it.** In the CPU-driven model a reflection re-walks + re-submits the
  scene (doubling the submission cost that is the documented bottleneck). Through the Phase-4 cull path
  a reflection is ≈ one extra cull dispatch + one indirect draw, reusing the retained buffers with zero
  re-upload.
- Add the **water surface as an extra cull plane** so below-water instances are rejected at instance
  granularity; flip winding for the mirrored pass. Composite over the Phase-3 sky reflection where rays
  miss geometry.
- **Cost caveat:** GPU-driven removes the *submission* half, not the *raster* half — the reflected view
  still rasterizes/shades a scene. Mitigate with half-res, reduced draw distance, opaque-only.

---

## 3. Plan → phase index

| Plan | Phase(s) | Role |
|---|---|---|
| [water-cdlod-geometry-plan.md](water-cdlod-geometry-plan.md) | 1 | Flat GPU water + shared CDLOD util. No infra deps. |
| [water-rendering-plan.md](water-rendering-plan.md) | 1 (waves, opt.), 3 (depth/refract/4a/look), 5 (4b) | The water look; split across phases by dependency. |
| [depth-prepass-plan.md](depth-prepass-plan.md) | 2 (keystone) | Opaque (incl. foliage) depth **+ view-space normal** prepass — early-Z today; sampleable depth for water/Hi-Z/Forward+, normal G-buffer for SSAO/GTAO/contact shadows. Unconditional (no flag), MSAA-ready. |
| [forward-plus-plan.md](forward-plus-plan.md) | 2 (parallel) | Clustered lighting; consumes the prepass. Forward (not deferred) to keep MSAA/A2C. |
| [gpu-object-rendering-plan.md](gpu-object-rendering-plan.md) | 4 (Stage 3) | Retained GPU objects + frustum cull + indirect = multi-view foundation. |
| [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) | 4 | **Concrete.** Retained GPU scene + cull/LOD compute + indirect (portable, Metal-safe) + Hi-Z occlusion. The GPU-driven keystone. |
| [compute-skin-bake-plan.md](compute-skin-bake-plan.md) | 4 (with object Stage 4) | Skinned meshes instancing-ready. |
| [cascaded-shadow-map-plan.md](cascaded-shadow-map-plan.md) | Tier 1 now; Tier 2 with 2 & 4 | CSM range→~1 km + 8 cascades @2K + multiview render (1..N/pass, DX12 hard-4). Cascade boxes are cull views (Phase 4); SDSM-lite tight fit off the prepass (Phase 2). |
| [rendering-performance-plan.md](rendering-performance-plan.md) | 1–4 (umbrella) | Draw-call/upload overhead; meets this roadmap at the indirect stages. |
| [hdr-pipeline-plan.md](hdr-pipeline-plan.md) / [procedural-sky-plan.md](procedural-sky-plan.md) / [terrain-conform-vegetation-roads-plan.md](terrain-conform-vegetation-roads-plan.md) | upstream | Landed / in-flight look work this roadmap builds on. |

---

## 4. Load-bearing principles (apply across plans)

1. **The dependency is the depth *prepass*, not all of Forward+.** Never gate water rendering on the
   clustered-lighting effort.
2. **Multi-view is the unifying capability.** Planar reflections and shadow cascades are both "render an
   arbitrary view cheaply." Build the prepass (Phase 2) and cull/indirect path (Phase 4) to take an
   arbitrary camera + optional clip plane; reflections then fall out nearly for free.
3. **Waves have no infra dependency** — the highest-visual-payoff water feature can land in Phase 1.
4. **Keep water's depth source swappable** so Phase 3 is not hard-blocked on Phase 2.
5. **The shared CDLOD util localizes the C++→Rust/GPU migration** to one site in Phase 4.
6. **Reversed-Z everywhere.** Froxel z-slicing (Forward+), Hi-Z reduction (`min`, culling plan), and
   water seabed-depth reconstruction all share the same reversed-Z hazard — derive against the actual
   projection, don't copy conventional-depth math.
7. **GPU-driven cuts submission, not rasterization.** Budget the extra raster of any added view.
8. **Stay MSAA-ready — the renderer is forward for a reason.** MSAA (and alpha-to-coverage foliage) is a
   primary intended capability; keeping shading *forward* (not deferred) is what preserves it. No plan may
   foreclose MSAA: `sample_count` is a pipeline parameter, sampleable depth resolves via `min` under MSAA
   (prepass §5), cutout foliage uses the *same* technique (hard discard / A2C + derivative rescaling) in
   the prepass and colour pass so it lands in the G-buffer, and Forward+ shades per-pixel. Nothing is
   built yet — the constraint is "don't paint us into a no-MSAA corner."
9. **No hand-editing the existing assets.** Any technique is fair game *as long as it needs no manual
   rework of the 25-year-old art* — procedural/load-time computation is fine (per-vertex AO baked at
   load, distance transforms of existing alpha, GPU-derived normals), hand-authored AO / re-authored
   textures / re-rigged models are not. This is what makes SSAO/GTAO + screen-space contact shadows the
   right path for foliage AO (they read the G-buffer, touch no assets) and rules out authored vertex AO.
