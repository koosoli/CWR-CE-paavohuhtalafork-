# Plan: move per-frame terrain conform off the CPU (vegetation + roads)

**Status:** proposed (2026-07-05). Expands Stage 1b of
[rendering-performance-plan.md](rendering-performance-plan.md). Two *different*
per-frame costs are conflated under "draping"; they need two different fixes.

## Two problems, not one

| | Vegetation (`ForestPlain`) | Roads / paths (pure `OnSurface`) |
|---|---|---|
| Symptom | `wgr_3d_vbuf` region overwritten ×N per frame (one write per instance), one draw each | per-frame CPU `SurfaceSplit` rebuild; **no** VB upload |
| Buffer? | Yes — one **shared** `VBDynamic` template mesh, re-uploaded per instance | **No VB** — excluded at `ShapeDraw.cpp:349`; drawn from a transient CPU tlTable |
| Deform | one **bilinear plane** per object (`ForestPlain::Animate`, `ObjectClasses.cpp:512`) | per-vertex `SurfaceY` + face clip against terrain grid (`FaceArray::SurfaceSplit`, `ClipShape.cpp:302`) |
| Static per level? | Yes (forest never moves, terrain static) | Yes (road never moves, terrain static) |
| Correctness bug today | `ForestPlain::Deanimate` is **empty** → all instances sample the *last* instance's deform | none (rebuilt fresh each frame) |
| Fix | precompute plane → per-instance SSBO → evaluate in vertex shader; base mesh static → instancing | conform **once at level load**, keep resident (cache split, or bake static VB) |

The already-shipped FNV-1a hash skip in `VertexBufferWgpu::Update`
(`EngineWgpu.cpp:165`) helps *single-instance mixed-clip draped props* (a
distinct draped object is byte-identical across frames → uploaded once). It does
**not** help vegetation (many instances, one buffer, different bytes each write)
and is irrelevant to pure roads (no VB at all). Correct the memory note that said
it "eliminated road re-uploads" — it eliminated *single draped-prop* re-uploads.

---

## Part A — Vegetation: per-instance conform plane in the vertex shader

> **Status: IMPLEMENTED 2026-07-05 (Option 2, exact two-triangle). Builds green
> (rwdi), shader composes; not yet visually/RenderDoc verified.** `ConformPlane` +
> `GCurrentConformPlane` (Shape.hpp), published once-cached by `ForestPlain::Draw`
> (`ComputeConformPlane`). `WgrDraw3D` gained `conform0/1/2`; the world SSBO element
> widened to `ObjectGpu` (mat4 + 3×vec4); `vs_main` conforms per instance and shears
> the normal. `EngineWgpu::Update` uploads `OrigPos`/`OrigNorm` (`BuildOrigVertices`)
> when a plane is active, so the shared undeformed mesh hashes identically across
> instances → one upload. GL33 untouched.

### What the CPU does today (the model we must reproduce exactly)

`ForestPlain::Animate` (`engine/Poseidon/World/Scene/ObjectClasses.cpp:512-641`)
for a non-`t1/t2` forest square:

1. Takes the object's **land-grid cell** (from `Position().X/Z * InvLandGrid`,
   floored to `x,z`; `xf=x, zf=z`).
2. Samples the four fine-heightmap corners of that land cell:
   `y00 = GetHeight(zs,xs)`, `y01`, `y10`, `y11` where `xs=x*subdiv`,
   `subdiv = 1 << (TerrainRangeLog - LandRangeLog)`.
3. Builds a two-triangle plane over the cell:
   ```
   d1000 = y10 - y00;  d0100 = y01 - y00;
   d1011 = y10 - y11;  d0111 = y01 - y11;
   xIn = worldX * InvLandGrid - xf;   zIn = worldZ * InvLandGrid - zf;   // may exceed 0..1 (extrapolated)
   y   = (xIn <= 1 - zIn) ? y00 + d1000*zIn + d0100*xIn
                          : y10 + d0111 - d1011*xIn - zIn*d0111;
   ```
4. Displaces every vertex: `world.y = y + model_pos.y + BoundingCenter().Y()`,
   with `world.xz` unchanged (non-rotated) or the full transform (rotated).
   `Deanimate` is a no-op.

The 4 corner heights depend only on `Position` and the (static) heightfield → the
**plane is constant for the level**. That is the whole leverage: compute it once,
never touch the vertex buffer again.

### Design

**Base mesh becomes static.** Upload the undeformed (`OrigPos`) forest mesh once.
Identical forest-square shapes across the map then share one vertex/index buffer →
they bucket into a single instanced draw (folds into Stage 3). No per-frame vertex
rewrite, no per-frame upload, and the last-write bug disappears (every instance
evaluates its own plane).

**Per-instance data** added to the Rust `Object` storage struct
(`gfx3d/mod.rs`, mirrored in `shader3d.wgsl`), one `DrapePlane` (32 B, two `vec4`):

| field | source |
|---|---|
| `xf, zf` | land-cell origin (`x`, `z` from `ObjectClasses.cpp:536-537`) |
| `y00, y10, d1000` | plane terms |
| `d0100, d1011, d0111` | plane terms |
| `drape_bias` | `BoundingCenter().Y()` |
| `mode` | 0 = none, 1 = forest plane (packed into a spare slot / sign bit) |

**Global uniform:** `inv_land_grid` (one `f32`, constant per level) folded into the
frame/camera UBO (`frame.wgsl`).

**Vertex shader** (in `vs_main`, gated on `obj.mode == 1`):
```wgsl
let wp   = obj.world * vec4<f32>(in.pos, 1.0);          // full model→world
let xIn  = wp.x * frame.inv_land_grid - obj.xf;
let zIn  = wp.z * frame.inv_land_grid - obj.zf;
let py   = select(obj.y10 + obj.d0111 - obj.d1011*xIn - zIn*obj.d0111,
                  obj.y00 + obj.d1000*zIn + obj.d0100*xIn,
                  xIn <= 1.0 - zIn);
let world_pos = vec3<f32>(wp.x, py + in.pos.y + obj.drape_bias, wp.z);
```
(For non-draped instances `mode==0`, use `wp.xyz` unchanged — a cheap per-instance
branch; forest draws bucket separately anyway so the branch is coherent.)

**Normals (refinement, second cut).** The CPU recomputes normals after deform; the
deformation is a Y-shear whose gradient is exactly the `t1/t2` skew
(`ObjectClasses.cpp:462-470`): `skewX = ±d0100/±d1011 * InvLandGrid`,
`skewZ = ±d1000/±d0111 * InvLandGrid` (sign per triangle half). Apply the
inverse-transpose shear to the normal: `n' = vec3(n.x - skewX*n.y, n.y, n.z - skewZ*n.y)`.
First cut: keep model normals (vegetation is mostly alpha cross-quads lit flat) and
verify visually before adding the skew.

### Engine integration (the fiddly part)

`EngineWgpu::DrawSectionTL` is generic (`Shape`-level) and doesn't know a draw is a
forest. Options to plumb per-object plane data to the backend:

- **(A, recommended) Global "current drape plane," set around the object's draw.**
  Mirror the existing `GSectionFilter` idiom: a `GCurrentDrapePlane` (mode + coeffs)
  set by `ForestPlain::Draw` before `Shape::Draw`, cleared after, and copied into
  `WgrDraw3D` when the command is recorded. Minimal, matches engine conventions.
- (B) Extend `BeginMeshTL` / the T&L mesh-begin call with drape params. More typed,
  but touches the shared T&L interface used by GL33.

Recommend **(A)**. `ForestPlain` computes the plane **lazily once** and caches it on
the object (invalidate never, currently — terrain is static). When wgpu drape-mode
is enabled it **skips the per-vertex loop** in `Animate` entirely and just publishes
the cached plane; the mesh stays at `OrigPos` and is uploaded once.

**GL33 path is untouched:** keep the CPU `Animate` deform for GL33 (and as the
fallback when the drape feature flag is off). Gate on a backend/feature check so both
renderers stay correct.

### Work items (Part A)
1. Rust: add `DrapePlane` to `Object` struct + `shader3d.wgsl`; add `inv_land_grid`
   to the frame UBO; implement the plane branch in `vs_main` (and `vs_skinned` no-op).
2. FFI: extend `WgrDraw3D` with the drape fields (`wgpu_renderer.h/.hpp`, `ffi.rs`).
3. Engine: `GCurrentDrapePlane`; `ForestPlain` computes+caches plane, publishes it,
   skips the CPU vertex loop under the wgpu drape flag; mesh uploaded once (static).
4. Verify in RenderDoc: forest `wgr_3d_vbuf` writes drop to one-per-shape; forests
   collapse toward one instanced draw per shape after Stage 3.

---

## Part A2 — Individual vegetation (ObjectPlain / ClipLand): per-vertex GPU conform

**Status: COLOR + SHADOW PASSES IMPLEMENTED 2026-07-05 (Option B, exact per-vertex).
Built + deployed (rwdi). Color pass user-verified (storm collapsed, no visual regression;
~a couple dozen residual vbuf copies remain — see memory). Shadow pass built+deployed,
pending user RenderDoc/visual verification.** Also folded in the "compute once" fix:
forests + individual veg skip the per-frame CPU vertex deform in BOTH the color and shadow
passes (gated on `GCurrentConformPlane.active`), keeping only the cheap bbox. GL33 (which
never publishes a plane) keeps deforming on the CPU (wgpu-first).

**Shadow pass (2026-07-05):** `surface_y` extracted into a shared naga_oil module
`shaders/conform.wgsl` and imported by both `shader3d.wgsl` and `shadow_depth.wgsl` (one
definition — color/shadow/gameplay can't diverge). `shadow_depth.wgsl`: `PassData` gained
`cam_pos` (casters are camera-relative → abs xz = caster.xz + cam_pos.xz); `CasterData`
gained `conform0.x` (bcSurfaceY) + `conform2.z` (mode); group(4) reuses the SAME
`ConformGroup`; `conform_pos()` in `vs_solid`/`vs_alpha` mirrors `vs_main` mode 2. The
color-pass publish logic became `Object::PublishConformPlane(sShape, saved)`, called by
both `Object::Draw` and `SceneShadowPass` (around Animate/AddShadowCaster/Deanimate).
`ConformGroup` now owns a 1×1 dummy heightmap so its bind is always valid (shadow pipelines
+ the `shadow_depth_probe` require group(4)). FFI: `WgrShadowCaster` +conform0/conform2
(104→136), `WgrShadowPass` +cam_pos (272→288), `WgrFrame` 512→528. Only mode 2 is needed
in the shadow shader (forests don't cast through this path: `IsAnimatedShadow=false`).

Landed: terrain `heightmap_view()`/`conform_params()` + `TerrainConformParams`; gfx3d
5th bind group `ConformGroup` (heightmap R32Float @0 + params UBO @1, VERTEX) added to
both mesh pipeline layouts, `max_bind_groups` raised to 5, bound in `draw_one`; WGSL
`surface_y` (== terrain `sample_height` == `SurfaceY`) + `vs_main` mode split (1 = forest
plane, 2 = per-vertex heightmap); per-vertex `conform` selector added to the vertex format
(`SVertex`/`WgrMeshVertex` 32→36 B, `@location(5)`, set from `OrigClip` in
`BuildOrigVertices`); `ConformPlane.mode`/`bcSurfaceY`; `GGpuTerrainConform` (set by
EngineWgpu); base `Object::Draw` publishes mode 2 (bcSurfaceY = SurfaceY at bounding
centre) for ClipLand shapes; `Object::Animate` ClipLand branch skips the deform when
active. Non-ClipLand meshes are safe under an inherited mode 2 (per-vertex selector 0 =
rigid). Normals: model normals for mode 2 (refinement pending).

**Original planning notes (Option B, exact per-vertex match to GL33/CPU):**

Individual trees/bushes are generic `ObjectPlain` (no dedicated class), conformed by
base `Object::Animate` (`Object.cpp:377-427`) — **genuinely per-vertex `SurfaceY`**,
with **per-vertex clip flags** (`ClipLandKeep` / `ClipLandOn` / rigid can differ within
one object). Hundreds of instances share one `LODShape` template (the `ShapeBank`
cache), so each rewrites the shared buffer per instance → the same storm as forests,
but the `ConformPlane` (single-plane) trick is insufficient because the conform is
per-vertex and per-vertex-clipped.

**Dual storm.** Color: `Object::Draw`→`Animate` per instance + `InvalidateBuffer`.
Shadow: `SceneShadowPass.cpp:526-530` calls `Animate(geomLOD)` per instance (non-static
objects) then `AddShadowCaster`→`buf->Update` re-uploads the shared shadow mesh. Plus
per-caster `wgr_shadow_caster` UBO writes.

**Gameplay coupling (why exact matters).** Occlusion / line-of-sight / fire geometry
run the SAME `Object::Animate` conform on the geometry LODs at intersection time
(`ObjectIntersect.cpp:753` → `AnimateComponentLevel` → `Animate`, `Object.cpp:529`). So
the visual conform must match the CPU conform exactly, or the rendered tree drifts from
the gameplay occluder. The GPU sample must equal `GLandscape->SurfaceY`; the terrain
shader's `sample_height` already matches it (same triangle interpolation), and since the
rendered terrain uses it, vegetation conformed with it sits on the visible ground.

### Design (exact, per-vertex)
1. **Bind the terrain heightmap (R32Float, already GPU-resident) + params into the
   object color pass** — group(0) `VERTEX` visibility (raise `max_bind_groups` if a 5th
   group is cleaner than a binding-7 add). Reuse `sample_height`/`hm_load` from
   terrain.wgsl (== `SurfaceY`).
2. **Per-vertex conform clip attribute** in the mesh vertex (`0`=rigid, `1`=LandKeep,
   `2`=LandOn), from `OrigClip(i) & ClipLandMask`. It's a per-model property → uploaded
   once with the shared `OrigPos` mesh. Placed at a vertex `@location` the skinned path
   (bones@3, weights@4) doesn't use, so characters are unaffected. +4 B/vertex globally.
3. **`vs_main` per vertex:** `Keep → y = SurfaceY(xz) + undeformedWorldY − bcSurfaceY`;
   `On → y = SurfaceY(xz)`; else rigid. Per-instance (in the ObjectGpu conform slots):
   a conform mode and `bcSurfaceY` (= `SurfaceY(bCenter.xz)`, precomputed once — static
   per object). xz unchanged. Normal recomputed from the local `SurfaceY` gradient
   (finite-difference) to match the CPU's post-deform `InvalidateNormals`.
4. **Shadow depth pass:** bind the heightmap there too; conform in `vs_solid`/`vs_alpha`
   (needs the clip attribute + `cam_pos` for absolute xz); extend `WgrShadowCaster` with
   the conform fields; publish the conform around `AddShadowCaster` so `buf->Update`
   uploads `OrigPos`.
5. **Engine publish:** general `ObjectPlain` with `ClipLand` hints publishes {mode,
   bcSurfaceY} around `Object::Draw` (color) and around `AddShadowCaster` in
   `SceneShadowPass` (shadow), via the `GCurrentConformPlane` global (extended). Keep the
   CPU `Animate` (bbox + gameplay geometry LODs untouched).
6. **Coalesce `wgr_shadow_caster` UBO** into a storage buffer indexed by instance_index
   (Stage-1 style) — removes the per-caster "layout" writes.

The forest `ConformPlane` (Part A, mode 1, land-grid plane) stays as-is; ClipLand
vegetation uses the heightmap path (modes 2/3, fine terrain `SurfaceY`). Both upload
`OrigPos` once and conform on the GPU.

**Sequence:** color pass → shadow pass → UBO coalescing (each buildable/verifiable).

## Part B — Roads / paths: conform once at level load, keep resident

### What the CPU does today

Pure `OnSurface` roads (`RoadType`, `ObjectClasses.cpp:668`) set
`OnSurface | ClipLandOn` on every vertex and are **excluded from vertex buffers**
(`ShapeDraw.cpp:349-352`). Each frame `Object::Draw` routes them through the shadow
cache split (`Object.cpp:930`) and `FaceArray::Draw` runs
`SurfaceSplit(...)` (`ClipShape.cpp:235-242`) — clipping every road face against the
terrain grid squares and conforming each resulting vertex with `SurfaceY`
(`ShapeLand.cpp:139`) into a transient `tlTable`, drawn immediately. Pure CPU,
rebuilt from scratch every frame, allocations and all.

The output depends only on the (static) heightfield → constant per level.

### Design

The geometry is static, so compute the split+conform **once** and reuse it. Two
increments:

- **B1 (low-risk first step): memoize the `SurfaceSplit` result.** Cache the
  conformed split faces on the road shape; on later frames skip `SurfaceSplit` and
  replay the cached geometry. Keeps the existing OnSurface draw path and depth
  handling; just removes the per-frame clip/conform/alloc.
- **B2 (end state, batching-friendly): bake to a resident static VB at load.** Run
  the conform once on a **level-load job** (engine job system / `FileServer`
  workers; terrain is available post-load), produce a static vertex+index buffer,
  and draw roads as ordinary static geometry — dropping them from the per-frame
  OnSurface path so they can sort/batch in Stage 2. Must preserve the road depth
  bias (`ZRoadEpsilon`, `SetBias(0x10)`) as pipeline state so roads still resolve
  above terrain without z-fighting.

Recommend shipping **B1** first (safe, immediate CPU win), then **B2** when we want
roads in the batched pipeline.

### Keep these on the per-frame path (genuinely dynamic)
- **Tyre tracks** (`Tracks.cpp:199`, `TrackClip` includes `ClipLandOn`) — a live
  ribbon that grows as vehicles move. Not static; must keep rebuilding.
- **Bridges** — have a geometry level, are *not* land-following, and use
  `DestructBuilding` (destructible). Ordinary objects; unaffected.

### Work items (Part B)
1. B1: add a per-shape conform cache keyed to "terrain generation"; populate on
   first draw, replay thereafter; bypass `SurfaceSplit` on cache hit.
2. B2: level-load conform job → static VB registration → remove static roads from
   the OnSurface per-frame branch; carry the road depth-bias pipeline state.
3. Confirm no runtime terrain deformation exists that would invalidate the cache
   (searches found none — `SurfaceY`/height array is read-only at runtime).

---

## Sequencing & relationship to the perf plan
- Part A is the bigger win (kills the vegetation upload storm **and** a correctness
  bug) and is the natural on-ramp to **Stage 3 instancing** — do it first.
- Part B (B1) is a cheap, independent CPU win; B2 lands roads in the **Stage 2**
  sort.
- The GPU-resident `R32Float` heightmap (already bound in the terrain/shadow passes,
  see rendering-performance-plan) is **not required** for either fix here (both
  precompute on the CPU). Keep it in reserve for any future *per-vertex* fine-terrain
  draping (large decals, or if we ever drop the forest plane approximation).

Related: [rendering-performance-plan.md](rendering-performance-plan.md),
[terrain-sun-shadows-plan.md](terrain-sun-shadows-plan.md).
