# Plan: GPU CDLOD water surface (geometry port)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** PLANNED (2026-07-08)
**Roadmap slot:** Phase 1 (no infra dependencies) — see [implementation-roadmap.md](implementation-roadmap.md).

> This is the **infrastructure** half of the water rework: replace the legacy per-segment mesh water
> with a GPU vertex-shader CDLOD water surface that mirrors the terrain system, and disable the legacy
> water path on the wgpu backend. It deliberately stops at a *flat, GPU-driven, frustum-culled,
> tessellated* water plane — enough to kill the frame hitches and unblock rendering work. The look
> (waves, transparency, refraction, planar reflections, per-map colour) is the sibling plan
> [water-rendering-plan.md](water-rendering-plan.md), which builds on the surface this one produces.
>
> **Prime directive: zero gameplay impact.** Everything in this plan is rendering-only. All the
> gameplay-relevant water reads (sea level, buoyancy, ray/sea intersection, unit placement) stay on the
> CPU in `Landscape` and are never touched. See §5.

---

## 0. Where we are today (verified 2026-07-08)

**Legacy water = per-segment `Shape` mesh, CPU-generated, animated-normal-map textured.**

- Water has **no** dedicated class. It is a secondary mesh (`LandSegment::_wTable`, a `Shape`) generated
  per **8×8 land-cell segment** (`LandSegmentSize = 8`, [LandscapeShared.hpp:18](../../Poseidon/World/Terrain/LandscapeShared.hpp#L18)),
  cached in the same `LandCache` as terrain. `LandSegment` at [Landscape.hpp:216](../../Poseidon/World/Terrain/Landscape.hpp#L216)
  holds `_wTable`, `_someWater`/`_onlyWater`, and the `_seaLevel` the tile was baked at.
- **Where water exists:** in `Landscape::GenerateSegmentInto` ([LandscapeRender.cpp](../../Poseidon/World/Terrain/LandscapeRender.cpp)),
  a segment gets water when its terrain min height `lMin <= maxTide + maxWave` (~5.25 m below datum),
  and per-quad a water face is emitted where the min of the 4 terrain corner heights dips that low
  (`LandscapeRender.cpp:793-1050`). Constants `maxTide = 5`, `maxWave = 0.25`
  ([LandscapeShared.hpp:15-16](../../Poseidon/World/Terrain/LandscapeShared.hpp#L15)). Off-map tiles are
  all-water.
- **Sea depth is encoded in the terrain heightmap as negative heights** — there is no separate water-depth
  field. The seabed is just terrain below `y = seaLevel`, and it is drawn normally by the (already
  GPU) terrain path; the water mesh is drawn on top.
- **The "wave" system is one global animated scalar.** `Landscape::Simulate`
  ([Landscape.cpp:727-748](../../Poseidon/World/Terrain/Landscape.cpp#L727)) sets
  `_seaLevel = maxTide * tide` (sun+moon-driven tide) and
  `_seaLevelWave = _seaLevel + sin(2π·time·_seaWaveSpeed) * maxWave` — the *entire* ocean is one flat
  plane bobbing ±0.25 m. `GetSeaLevel()` returns `_seaLevelWave`
  ([Landscape.hpp:795](../../Poseidon/World/Terrain/Landscape.hpp#L795)). There is **no per-vertex wave
  geometry** — only a texture-scroll shimmer (`MoveWater`, [LandscapeShared.hpp:21](../../Poseidon/World/Terrain/LandscapeShared.hpp#L21))
  and an animated `.pac` (`data\more_anim.01.pac`, forced in `SetTexture(0,…)`,
  [Landscape.cpp:1332](../../Poseidon/World/Terrain/Landscape.cpp#L1332)).
- **Draw path:** `Landscape::DrawWater` ([LandscapeRender.cpp:1146](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1146))
  iterates segments, translates each `_wTable` by `Offset() + (0, _seaLevelWave, 0)`, animates the
  texture, and submits it as a generic engine `Shape`. Called from `Landscape::Draw`
  ([LandscapeRender.cpp:1551](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1551)) **after**
  `DrawGround` (opaque terrain, line 1533).
- **Terrain already went GPU; water did not.** `Landscape::DrawGround` delegates to
  `GEngine->GetTerrainRenderer()` when non-null and skips the legacy Shape path
  ([LandscapeRender.cpp:1275](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1275)). The wgpu backend
  returns a `TerrainWgpu` there ([EngineWgpu.cpp:513](../EngineWgpu.cpp#L513)); GL33 returns `nullptr`
  ([Engine.hpp:941](../../Poseidon/Graphics/Core/Engine.hpp#L941)). **`DrawWater` has no such hook** —
  there is no `IWaterRenderer`, so on wgpu the legacy `_wTable` still routes through the generic mesh
  pipeline and is effectively broken/unrendered, while every regeneration on sea-level change is a CPU
  hitch source.

**The terrain GPU CDLOD path is the exact template to copy:**

- `TerrainWgpu : public ITerrainRenderer` ([TerrainWgpu.cpp](../TerrainWgpu.cpp)) owns a flat CDLOD
  quadtree (`BuildQuadtree`/`BuildNode`, `.cpp:32-107`) over `TerrainGridN`(=32)-texel leaves, each node
  carrying `originX/Z`, `size`, `level`, `child[4]`, and a scanned `minY/maxY`. Per frame `DrawTerrain`
  (`.cpp:255`) clips to the visible rect, runs `SelectCdlod` (frustum + distance descent,
  [TerrainCdlod.hpp:72](../../Poseidon/World/Terrain/TerrainCdlod.hpp#L72)) with morph bands
  (`CdlodMorphBand`, `.hpp:49`), emits `WgrTerrainNode{origin,size,lod,morph_start,morph_end}`, and calls
  `EngineWgpu::SubmitTerrain` ([EngineWgpu.cpp:1282](../EngineWgpu.cpp#L1282)).
- `SubmitTerrain` packs nodes into `WgrFrame.terrain_nodes` + a `WgrTerrainBatch` and pushes a
  `WGR_CMD_DRAW_TERRAIN` command. The Rust side (`terrain/mod.rs`) uploads them as instance-step vertex
  data over a shared 32×32 unit grid + skirt ([terrain/mod.rs:1260](../rust/src/terrain/mod.rs#L1260)),
  displaces each vertex by an `R32Float` heightmap sampled **in the vertex shader**, and morphs toward
  the coarser lattice by camera distance ([terrain.wgsl:129-168](../rust/src/terrain/terrain.wgsl#L129)).
- FFI structs `WgrTerrainNode` (24 B) / `WgrTerrainParams` (32 B) are declared identically in
  [wgpu_renderer.hpp](../include/wgpu_renderer.hpp) and [ffi.rs](../rust/src/ffi.rs) with size
  `static_assert`s.

---

## 1. Design decisions

1. **Mirror the terrain path exactly; do not fold water into terrain.** Add a parallel `IWaterRenderer`
   interface + `WaterWgpu` implementation + `water/mod.rs` module + `WgrWaterNode`/`WgrWaterParams` FFI +
   `WGR_CMD_DRAW_WATER`. Water and terrain differ enough (transparency, no height displacement in this
   plan, a different existence test, a per-frame animated sea level, and — in the sibling plan — an
   entirely different shader with reflection/refraction) that a shared *pipeline* would be a tangle.

1b. **Extract the shared CDLOD driver rather than copy-pasting `TerrainWgpu`.** The pure selection
   algorithm ([TerrainCdlod.hpp](../../Poseidon/World/Terrain/TerrainCdlod.hpp)) is already header-only
   and generic (`SelectCdlod` is templated on visible/emit functors). The two pieces that are *not* yet
   shared — the tree build (`BuildNode`/`BuildQuadtree`, [TerrainWgpu.cpp:32-111](../TerrainWgpu.cpp#L32))
   and the per-frame driver (rect clip + `Camera::IsClipped` visible-lambda + `SelectCdlod` + emit,
   [TerrainWgpu.cpp:255-316](../TerrainWgpu.cpp#L255)) — should be factored into a small reusable pair,
   e.g. `BuildCdlodTree(range, grid, boundsFn, includeFn)` and
   `SelectVisibleCdlod(tree, camera, rect, morph, emit)`, that **both** `TerrainWgpu` and `WaterWgpu`
   drive with functors. Water passes a below-sea `includeFn` (§1.2) and an emit that produces
   `WgrWaterNode`. **Do not build a general `QuadTree<T>`** — the CDLOD node is specific and the one
   genuinely generic piece already exists. Keep the shared driver **in C++ for now**; it is exactly what
   makes the eventual move of selection to Rust/GPU (Phase 4,
   [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)) a **one-site** refactor instead of a
   two-copy one.

2. **The water quadtree covers only below-sea-level regions, generously.** Reuse the per-node `minY`
   already scanned in `BuildNode`. Include a node in the water tree when
   `minY <= seaLevelDatum + maxTide + maxWave + margin` (same threshold the legacy `_someWater` test uses,
   plus a small margin for wave crests). Off-map is water. **Precision at the shoreline is not needed** —
   see decision 3 — so the tree can be coarse and conservative.

3. **Let the depth buffer cut the shoreline; don't mesh it.** The water is a flat plane at
   `y = seaLevel`. Terrain (the seabed and the coast) is drawn first, into the same reversed-Z depth
   buffer. Water is drawn depth-tested (`GreaterEqual`): wherever the coast rises above the water plane,
   the terrain is nearer and occludes the water fragments behind it, producing an exact waterline for
   free — no per-tile shoreline geometry, no CPU regeneration on tide change. This is the single biggest
   simplification over the legacy per-quad mesh and the main reason the hitches disappear.

4. **Sea level is a per-frame global uniform, not baked geometry.** The animated `_seaLevelWave` scalar
   rides in `WgrWaterParams` each frame (it is one float; the whole plane is at that height). No mesh
   ever needs regenerating when the tide/wave scalar moves — the legacy code re-levels cached vertices
   every sea-level change ([LandscapeRender.cpp:1299-1309](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1299));
   we just change a uniform. Keep feeding the real `GetSeaLevel()` so the visual surface stays at the
   height gameplay floats objects at (buoyancy is unchanged — §5).

5. **Reuse terrain's grid + skirt + morph even though the plane is flat.** For a flat plane the morph and
   skirts are near no-ops, but keeping the identical mesh/morph machinery means the sibling plan's
   Gerstner vertex displacement drops straight in with crack-free LOD transitions. Cost is trivial.

6. **Render water in its own pass, after opaque, into the HDR scene target.** Emit a
   `WGR_CMD_DRAW_WATER` positioned after all opaque 3D + terrain within the scene segment, before the
   tonemap `Resolve`. In *this* plan the water can even be opaque (depth-write on) — a flat shaded plane
   — but wire the pipeline as transparent-ready (depth-test `GreaterEqual`, depth-**write off**, `Alpha`
   blend state, [gfx3d/mod.rs:1114](../rust/src/gfx3d/mod.rs#L1114)) so the sibling plan needs no
   pipeline surgery. Draw into `scene_view` (`Rgba16Float`) with the `linear` HDR override
   ([gfx3d/mod.rs:1226](../rust/src/gfx3d/mod.rs#L1226)).

7. **Bind the shared frame group(0).** Reuse `gfx3d.camera_layout()` so the water shader gets
   `frame.proj/view/cam_pos`, sun dir/colour, the froxel aerial-fog volume, and `apply_fog`/`fog_factor`/
   `reverse_z` via `#import frame::…` for free ([frame.wgsl](../rust/src/shaders/frame.wgsl)). Distant
   water then dissolves into the same procedural sky/horizon the terrain does.

8. **wgpu-only; GL33 untouched, with an A/B toggle.** GL33 keeps `GetWaterRenderer() == nullptr` and its
   legacy `_wTable` water. On wgpu, gate the new path behind an env flag (e.g. `WGR_GPU_WATER`, default
   on) so the legacy path can be re-enabled during bring-up, mirroring the sky plan's toggle.

---

## 2. Data + FFI surface

### 2.1 New structs (declare identically in [wgpu_renderer.hpp](../include/wgpu_renderer.hpp) + [ffi.rs](../rust/src/ffi.rs), with size `static_assert`s)

- **`WgrWaterNode`** — can be **byte-identical to `WgrTerrainNode`** (`origin: vec2`, `size`, `lod`,
  `morph_start`, `morph_end`; 24 B). Uploaded as instance-step vertex data. (Kept as a distinct type for
  clarity/independent evolution; the sibling plan may add a per-node flag.)
- **`WgrWaterParams`** — minimal for this plan: `world_origin: vec2` (terrain texel-0 world xz, for
  optional heightmap sampling), `terrain_grid: f32`, `hm_width/hm_height: u32` (to reuse the terrain
  heightmap for shoreline depth later), `sea_level: f32` (per-frame animated), plus a `_pad`. The
  look/colour fields (deep/shallow colour, clarity, wave params) belong to the sibling plan's
  `WgrWaterLook` uniform — keep this struct about *placement*, not appearance.

### 2.2 New FFI entry points ([ffi.rs](../rust/src/ffi.rs) + header)

- `wgr_water_set_params(renderer, const WgrWaterParams*)` — set/refresh placement params (called on map
  load and per-frame for `sea_level`, or split sea_level into the per-frame `WgrFrame` if cheaper).
- Per-frame nodes ride inside `WgrFrame` exactly like terrain: add `water_nodes: WgrSlice<WgrWaterNode>`
  and `water_batches: WgrSlice<WgrWaterBatch>` to `WgrFrame`
  ([ffi.rs:519-527](../rust/src/ffi.rs#L519)) and a `WGR_CMD_DRAW_WATER` kind to `WgrCmdKind`
  ([ffi.rs:428](../rust/src/ffi.rs#L428)). `WgrWaterBatch` mirrors `WgrTerrainBatch`
  (`first_node,node_count,camera,_pad`). Bump `WgrFrame`'s size `static_assert`.
- The water surface reuses the terrain **heightmap** that is *already uploaded* to the renderer — no new
  heightmap upload. The sibling plan reads it for depth/shoreline; this plan may ignore it entirely.

### 2.3 Rust side

- New module `engine/WgpuRenderer/rust/src/water/` (`mod.rs` + `water.wgsl`), a trimmed copy of
  `terrain/mod.rs`: shared 32×32 grid + skirt (`build_grid`), an instance buffer of `WgrWaterNode`, a
  params UBO, and one render pipeline. **No** bindless ground array, index map, jitter, or shadow-sweep —
  water needs none of that. `Water::prepare(nodes)` + `Water::draw(pass, cam_bind, cam_off, first, count)`
  copy the terrain equivalents ([terrain/mod.rs:1005-1057](../rust/src/terrain/mod.rs#L1005)).
- `Renderer` gains `water: Water`, `water_set_params`, and dispatch of `WGR_CMD_DRAW_WATER` in the
  segment replay (`Plan3dOp::Water` → `water.draw`), mirroring `Plan3dOp::Terrain`
  ([lib.rs:692-701](../rust/src/lib.rs#L692)). Slot the water op **after** the opaque/terrain ops in the
  plan so it draws over them.

### 2.4 C++ side

- New interface `engine/Poseidon/Graphics/Core/IWaterRenderer.hpp` — single method
  `DrawWater(Scene&, int xBeg,int zBeg,int xEnd,int zEnd)`, mirroring
  [ITerrainRenderer.hpp](../../Poseidon/Graphics/Core/ITerrainRenderer.hpp). Add
  `virtual IWaterRenderer* GetWaterRenderer() { return nullptr; }` on `Engine`
  next to `GetTerrainRenderer` ([Engine.hpp:941](../../Poseidon/Graphics/Core/Engine.hpp#L941)).
- In `Landscape::DrawWater` ([LandscapeRender.cpp:1146](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1146)):
  `if (IWaterRenderer* w = GEngine->GetWaterRenderer()) { w->DrawWater(scene, rect…); return; }` before
  the legacy loop — the exact shape of the `DrawGround` delegation. GL33 falls through to `_wTable`.
- New `WaterWgpu : public IWaterRenderer` in `engine/WgpuRenderer/WaterWgpu.cpp/.hpp`, driving the
  shared CDLOD helper (§1b): builds its tree with a below-sea `includeFn` (§1.2) and, in `DrawWater`,
  calls `SelectVisibleCdlod` with an emit that produces `WgrWaterNode`s, then `EngineWgpu::SubmitWater`.
  `TerrainWgpu` is refactored onto the same helper in the same change (its behaviour must not regress —
  it is the correctness reference). `EngineWgpu`
  creates `_water = std::make_unique<WaterWgpu>(…)` alongside `_terrain`
  ([EngineWgpu.cpp:513](../EngineWgpu.cpp#L513)) and overrides `GetWaterRenderer()`.
- `EngineWgpu::SubmitWater(std::span<const WgrWaterNode>)` mirrors `SubmitTerrain`
  ([EngineWgpu.cpp:1282](../EngineWgpu.cpp#L1282)): append nodes + a `WgrWaterBatch`, push
  `WGR_CMD_DRAW_WATER`; flush `frame.water_nodes/water_batches` at frame end
  ([EngineWgpu.cpp:1024](../EngineWgpu.cpp#L1024)). Push `sea_level` each frame from
  `GLandscape->GetSeaLevel()`.

---

## 3. Stages

Each stage is independently testable and leaves the tree working.

### Stage 0 — Interface + no-op hook
- Add `IWaterRenderer` + `GetWaterRenderer()` + the `DrawWater` delegation, plus an empty `WaterWgpu`
  whose `DrawWater` does nothing (behind `WGR_GPU_WATER`). Legacy water suppressed on wgpu.
- **Exit:** on wgpu, legacy `_wTable` no longer submitted (oceans render as empty/seabed); GL33 unchanged;
  no hitches from water regeneration. Confirms the suppression hook is correct before building anything.

### Stage 1 — CDLOD water quadtree + flat plane
- `WgrWaterNode`/`WgrWaterParams`/`WGR_CMD_DRAW_WATER` FFI; `water/mod.rs` with the shared grid pipeline;
  `WaterWgpu::BuildQuadtree` (below-sea test) + `DrawWater` (select/cull/emit) + `SubmitWater`.
- `water.wgsl`: VS places the grid at `(origin + grid*size, sea_level)`, camera-relative, `reverse_z`;
  FS outputs a flat sky-tinted colour (sample the froxel far slice or a constant deep-blue), fogged via
  `apply_fog`. Depth-test `GreaterEqual`, drawn after opaque.
- **Exit:** a flat, frustum-culled, distance-tessellated water plane fills every below-sea area on wgpu,
  correctly occluded by coastlines via depth test, no CPU hitches on tide/wave changes. Ugly but solid.

### Stage 2 — Correctness pass
- Verify against maps with large water (e.g. Everon/Malden coastlines, Kolgujev): waterline matches the
  legacy render at multiple tide phases; no z-fighting on near-flush shores (add a tiny depth bias or a
  small downward epsilon if needed); no cracks at LOD seams (skirts); far water meets the horizon/sky
  seamlessly; performance is flat (no per-frame allocation, instance buffer reused like terrain's
  [terrain/mod.rs:1016](../rust/src/terrain/mod.rs#L1016)).
- Confirm **gameplay untouched**: boats float, weapons splash, camera/horizon behave — all read
  `Landscape` sea level, which we never changed (§5).
- **Exit:** GPU water is a drop-in replacement for legacy water geometry; ready for the look overhaul.

---

## 4. Render-pass placement details

- Water draws into `scene_view` (HDR `Rgba16Float`) inside a scene segment, **after** the opaque 3D +
  terrain ops and **before** the `Plan3dOp::Resolve`/tonemap ([lib.rs:802](../rust/src/lib.rs#L802)), so
  it composites in linear radiance and gets tonemapped/bloomed with everything else.
- Depth: reversed-Z, `DEPTH_FORMAT = Depth24PlusStencil8`, compare `GreaterEqual`. Transparent-ready =
  depth-**write off** (so overlapping wave-tile edges in the sibling plan don't self-occlude); acceptable
  here because water is the last opaque-ish thing and nothing 3D draws behind it in-segment.
- The multi-segment depth-clear structure ([lib.rs:707-810](../rust/src/lib.rs#L707)) means water must be
  emitted in the segment whose depth contains the terrain it tests against — i.e. right after terrain in
  the same segment. Validate the command ordering in `EngineWgpu` produces that (terrain submit → object
  submits → water submit, all one camera/segment).
- The water pipeline takes the scene **`sample_count`** like every other pipeline, so it comes along for
  free when MSAA is enabled later (see [depth-prepass-plan.md](depth-prepass-plan.md) §5). No water-specific
  MSAA work — water is opaque-blended geometry, not alpha-to-coverage.

---

## 5. Gameplay isolation (the prime directive)

All of these stay **exactly** as they are — they read `Landscape`, entirely CPU, independent of which
renderer draws the surface. This plan changes only *how the water surface is drawn*.

- Sea height: `_seaLevelWave` / `GetSeaLevel()` ([Landscape.cpp:727](../../Poseidon/World/Terrain/Landscape.cpp#L727),
  [Landscape.hpp:795](../../Poseidon/World/Terrain/Landscape.hpp#L795)); helpers `SurfaceYAboveWater`,
  `RoadSurfaceYAboveWater`, `AboveSurfaceOrWater` ([Landscape.cpp:1942-2031](../../Poseidon/World/Terrain/Landscape.cpp#L1942)).
- Ray/sea intersection: `IntersectWithGroundOrSea` ([Collisions.cpp:2241](../../Poseidon/World/Simulation/Collisions.cpp#L2241))
  (weapons, camera, light projection).
- Buoyancy/underwater: `GroundCollisionPlane` → `GroundWater`/`info.under`
  ([Collisions.cpp:796-872](../../Poseidon/World/Simulation/Collisions.cpp#L796)); consumers in
  `Car.cpp`/`Tank.cpp`/`Helicopter.cpp`/`Ship.cpp`. Surface material/sound/splash: `GetWaterSurface()`.
- The global sea-level bob (`_seaLevelWave`) keeps driving both gameplay and the water plane height, so
  the visual surface and the float height agree. **We do not feed the sibling plan's cosmetic Gerstner
  waves back into gameplay** — buoyancy stays on the flat sea plane (waves are kept gentle so boats never
  visibly float in air; noted in the sibling plan).
- Only the *rendering-only* reads are dropped/replaced: the water-mesh offset/re-level
  ([LandscapeRender.cpp:1179,1299](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1179)) and the GL33
  water pass — both wgpu-inert.

---

## 6. Open questions / risks

- **Coast z-fighting at flush tide.** Where terrain sits almost exactly at sea level over a wide flat
  area, the water plane and seabed can z-fight. Mitigations: small constant depth bias on the water
  pipeline, or discard water fragments where the sampled terrain height (reuse the already-uploaded
  heightmap) is above `sea_level` — the latter also gives a crisp shoreline without relying purely on
  depth. Decide during Stage 2.
- **Off-map / world edge.** Legacy makes off-map all-water to the horizon. Decide the water tree's outer
  extent (out to the camera far plane / fog max, like terrain's cull rect
  [TerrainWgpu.cpp:276](../TerrainWgpu.cpp#L276)) so the ocean reaches the horizon without a hard edge.
- **Node count / batching.** Water can cover most of a coastal map at fine LOD near shore. Confirm the
  CDLOD `_lodRatio`/`_baseMult` tuning gives a sane node count; water can likely afford a coarser base
  than terrain since (this plan) it is flat. Env-tune like the terrain knobs
  ([TerrainWgpu.cpp:81-86](../TerrainWgpu.cpp#L81)).
- **Depth-write choice.** If a later opaque effect needs to depth-test against the water surface, revisit
  the depth-write-off decision. For now nothing does.
- **Sea level per-frame plumbing.** Choose whether `sea_level` lives in `WgrWaterParams` (a tiny
  per-frame `wgr_water_set_params` call) or is promoted into the per-frame `WgrCamera`/`WgrFrame` (one
  more float, no extra call). Prefer the latter if it's cheap.

---

## 7. Landing order

Stage 0 (hook + suppress legacy) → Stage 1 (CDLOD flat plane) → Stage 2 (correctness) — one PR each,
behind `WGR_GPU_WATER`. On completion, the wgpu backend has a robust flat GPU ocean and zero water-mesh
hitches; hand off to [water-rendering-plan.md](water-rendering-plan.md) for the visual overhaul.

**Optional pull-forward (recommended for a decent interim look):** the sibling plan's Gerstner waves +
sun specular (water-rendering Stage 1) have **zero** infra dependency — pure vertex displacement +
shading over this plan's grid, no depth prepass / culling / Forward+ needed. Landing them here (a Stage
1.5) gives a genuinely nice waved ocean before the big Phase-2/3 infra work. Prefer **"vastly simplified
but *waved*"** over reproducing the legacy animated-normal-map texture — that texture is throwaway. See
[implementation-roadmap.md](implementation-roadmap.md) Phase 1.
