# Plan: Terrain fractal detail — cached detail-height field, scaled normals + near-field displacement

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** PLANNED (2026-07-08). **Decision: build Stage 1 (scaled detail normals) first.** Stage 2
(near-field displacement) is opt-in / deferred — a visual-only bump the simulation can't see causes
sightline disagreement (false cover / phantom occluders), which normals avoid entirely.
**Roadmap slot:** independent of Phases 2–5, but the displacement half (Stage 2) *pairs with* the depth
prepass + SSAO/contact shadows ([depth-prepass-plan.md](depth-prepass-plan.md)) for its near-field
shadowing. See [implementation-roadmap.md](implementation-roadmap.md).

> Add high-frequency terrain detail **without** raising the gameplay grid. The engine's `setTerrainGrid`
> fractally subdivides the *real* heightmap — genuinely more detailed, but it changes gameplay physics
> (so MP locks the grid) and costs mesh/heightmap/shadow work. This plan instead bakes the fractal detail
> once into a **render-only detail-height field** (cached to disk) and applies it two ways, crossfaded by
> CDLOD LOD: **scaled detail normals** everywhere (cheap shading detail, no geometry) and **real vertex
> displacement** on near/fine tiles (real geometry → the depth prepass + SSAO/contact shadows react, so no
> false depth). Decoupled from the gameplay grid: no `PreferredGridSizeMP` lock, no physics change.

---

## 0. Where we are today (verified 2026-07-08)

- **`setTerrainGrid g` → `World::AdjustSubdivisionGrid(g)`** ([WorldImpl.cpp:1050](../../Poseidon/World/WorldImpl.cpp#L1050)):
  computes the doublings from the land grid to `g` (`log2(LandGrid/g)`, clamped 0–8) and subdivides up
  (`SubdivideTerrain`) or resamples down (`ResampleTerrain`) to hit it.
- **Subdivision is fractal midpoint displacement, not interpolation** (`SubdivideTerrainOneStep`,
  [LandSave.cpp:576](../../Poseidon/World/Terrain/LandSave.cpp#L576)): each step **doubles** the heightmap
  (`resultX = _terrainRange*2`) into a new `_data`, perturbing midpoints by the map's `CfgWorlds >>
  <world> >> Subdivision >> Fractal` config (`rougness`, `maxSlopeFactor`, …) scaled by local slope.
  **Gated off** where the cell is too flat, below `minY` (underwater), in forest, or **near a building /
  road** ([object scan, LandSave.cpp:688](../../Poseidon/World/Terrain/LandSave.cpp#L688)) — deliberately
  keeping terrain smooth under placed objects.
- **Cached to disk**: `LoadSubdivCache`/`SaveSubdivCache` → `cwr_subdiv_<map>_L<n>.cache`
  ([WorldImpl.cpp:1062](../../Poseidon/World/WorldImpl.cpp#L1062)) — a remaster addition (the fractal is
  original OFP). The bake machinery to extend already exists.
- **It affects gameplay.** The subdivided `_data` is the gameplay heightmap: `SurfaceY`
  ([Landscape.cpp:1546](../../Poseidon/World/Terrain/Landscape.cpp#L1546)), collisions, AI, placement all
  read it. Proof of intent: **MP forces a common grid** — `if (mode == GModeNetware) gridSize =
  PreferredGridSizeMP` ("MP requires all clients use the same grid size",
  [WorldImpl.cpp:1080](../../Poseidon/World/WorldImpl.cpp#L1080)). So raising the grid globally is not
  free: perf (bigger heightmap, more CDLOD nodes, more sun-shadow-sweep) **and** a physics/MP change.
- **wgpu terrain already shades from the heightmap per-fragment.** `fs_terrain` derives its normal by
  central-differencing the heightmap at `terrain_grid` step (`sample_normal`,
  [terrain.wgsl:77-83](../rust/src/terrain/terrain.wgsl#L77)); the VS displaces by `sample_height`
  ([terrain.wgsl:129-168](../rust/src/terrain/terrain.wgsl#L129)) with a `morph_k` LOD-morph factor
  already in hand. Detail normals + displacement slot directly into these.

---

## 1. Design decisions

1. **Render-only, decoupled from the gameplay grid.** The detail is a wgpu-terrain overlay; it does **not**
   mutate `_data`, so no physics change, no `PreferredGridSizeMP` lock, no gameplay cost. This is the whole
   reason to do it this way instead of a permanent `setTerrainGrid`.
2. **One cached "detail-height field" = the *difference*** between the fractal-subdivided height and the
   (upsampled) coarse height — the high-frequency component only. Mean-zero, small-amplitude (precision-
   and compression-friendly), fades cleanly to zero. Baked once, **cached to disk** (extend the existing
   `cwr_subdiv` cache to a `cwr_detail_<map>.cache`), uploaded to the renderer as a detail texture.
3. **Two applications of the same field, crossfaded by CDLOD LOD:**
   - **Scaled detail normals — everywhere, but reduced with range** (Stage 1). Shading detail with no
     geometry; scaled down at distance so it never implies depth that isn't shadowed.
   - **Real vertex displacement — near/fine tiles only** (Stage 2), faded in with the existing `morph_k`
     so it applies only where tessellation can represent it (else it aliases/pops).
4. **Near-field shadows come for free from the prepass, not the terrain sweep.** Displaced near geometry
   is real, so the unconditional **depth+normal prepass** captures it and **SSAO + screen-space contact
   shadows** react to its self-shadowing ([depth-prepass-plan.md](depth-prepass-plan.md)). The long-range
   terrain **sun-shadow sweep stays on the coarse heightmap** (it's for mountains, not cm bumps) — so no
   change there, and no false-depth problem in the near field.
5. **Derive the detail normal in-shader from the detail field** (one source of truth — the same field
   drives displacement and normals), scaled by `detail_strength(lod)`. A **baked normal map** is an
   optional later swap if the extra taps cost too much (Stage 3).
6. **Inherit the fractal's gating.** Because the bake runs the real `SubdivideTerrain`, the detail field is
   already zero on flat terrain, underwater, in forest, and under buildings/roads — exactly where you want
   *no* detail (and, for displacement, where placed objects must sit on smooth terrain).

---

## 2. The bake pipeline (C++/load-time)

- At load, run a **virtual** fractal subdivision (reuse `SubdivideTerrain` on a scratch copy — **do not**
  overwrite the gameplay `_data`) to the target detail level, then compute
  `detail[x,z] = fractal_height[x,z] − upsample(coarse_height)[x,z]`.
- Store as a single-channel field (e.g. `R16Float`, or scaled `R8` if amplitude is bounded), map-wide.
  Size is modest for these maps (a 256² coarse → 1024² detail R16 ≈ 2 MB). Cache to
  `cwr_detail_<map>.cache`; regenerate only when absent/stale.
- Upload to the wgpu terrain as a **detail-height texture** bound alongside the heightmap (new binding in
  the terrain group), with its world→texel mapping (origin + detail grid) in `WgrTerrainParams` or a small
  detail-params uniform. FFI mirrors the heightmap upload (`wgr_terrain_set_detail_height` or fold into
  the existing detail-layer path).
### 2.1 Storage sizing & escalation (single texture is almost always enough)
- **Default: one map-wide texture, sized to the device.** At load, cap the baked detail resolution to
  `min(device.limits().max_texture_dimension_2d, memory_budget)` and clamp the subdivision level to match;
  put the chosen resolution in the cache key (bake-and-cache makes device-sizing free). The renderer
  already queries and clamps against `max_texture_dimension_2d` for the heightmap
  ([terrain/mod.rs:676](../rust/src/terrain/mod.rs#L676)) — reuse that.
- **The math says it fits.** Guaranteed floor is **8192**, typical desktop **16384**. OFP coarse maps are
  ~256²–512²; 2–3 subdivision steps (ample detail) give 1024²–4096² — comfortably under the 8192 floor.
  You'd only approach the limit with extreme subdivision (4+ steps) or an unusually large custom map.
  **Memory binds before dimension:** 4096² RG8 ≈ 32 MB (fine), 8192² ≈ 128 MB (chunky) — so cap by budget,
  not just dimension.
- **Escalation, only if that rule can't be met** (huge map + high detail): a **camera-following detail
  clipmap** — one fixed-size scrolling window, toroidally updated (re-bake only the strip entering view),
  coarser mips outward. It leans on the fact that detail only matters near the camera (it fades with range
  anyway), keeps memory constant regardless of map size, and needs **no tile index** (`frac((world −
  clipOrigin)/clipSize)` with wrap). This is the right escalation, not a static tile array.
- **Last resort — a tile array** (`N²`-per-tile, row-major `ty*T+tx` index; a space-filling curve buys
  nothing for *addressing* — only for streaming/disk locality, where **Morton/Z-order** is the pragmatic
  pick, not Peano). Its real cost is **edge gutters** (1 texel for bilinear, `2^level` per mip) to stop
  cross-tile filtering seams — which is itself a reason to prefer the single texture or the clipmap.
- **Optional cheaper alternative** for extreme cases: a *tiling* detail-noise texture blended by slope,
  approximating the fractal without a map-wide bake — loses the map-specific gating, so only if a baked
  field genuinely won't fit. Not the default.

## 3. Shader integration (`terrain.wgsl`)

- **VS (Stage 2):** `height = sample_height(coarse) + sample_detail(world_xz) * displace_weight`, where
  `displace_weight` is ~1 at the finest LOD and →0 as the tile morphs coarse (reuse `morph_k`), and the
  detail is mip-selected to match the current tessellation (avoid representing frequencies the mesh can't).
- **FS:** combine the base normal (coarse central-diff, unchanged) with a **detail normal** (central-diff
  of the detail field) scaled by `detail_strength(lod, dist)` — full where geometry is real (near),
  reduced at range (Stage 1's scaled-normal regime). Keep the detail normal in the same space as the base
  (derive from the height gradient) so they compose seamlessly.
- The crossfade is continuous: near = real displacement + full-strength normals; far = no displacement +
  scaled-down normals. One `detail_strength`/`displace_weight` pair driven by LOD/morph ties them together.

## 4. Stages

### Stage 0 — Bake + cache + upload (no shading change)
- Virtual subdivide → detail-height field → `cwr_detail_<map>.cache` → upload as the detail texture.
  Validate the data (visualize the field) without changing rendering yet.
- **Exit:** the detail field exists, is cached, and is bound to the terrain — inert.

### Stage 1 — Scaled detail normals (recommended first; safe, cheap, no mismatch)
- `fs_terrain` samples the detail field for a detail normal, scaled by `detail_strength(dist)` so distant
  terrain stays subtle. **No geometry change**, so no physics/visual mismatch, no perf-geometry cost.
- **Exit:** terrain reads as far more detailed under lighting, at ~one extra texture tap, with zero
  gameplay impact. This is the bulk of the visual payoff.

### Stage 2 — Near-field real displacement (optional, richer, one caveat)
- VS adds the detail as real displacement on near/fine tiles, faded by `morph_k`; the detail normal ramps
  to full strength there. Near geometry becomes real → the depth+normal prepass + SSAO/contact shadows
  react (design decision 4).
- **Caveat (the honest one):** gameplay stays on the coarse `_data`, so displaced visual terrain diverges
  from the simulation in two ways: (a) **cosmetic** — a unit/vehicle placed at coarse height floats/sinks
  by up to the detail amplitude; (b) **sightline / line-of-sight disagreement (the sharper one)** — a
  visible crest or mound that the engine's LOS / weapon raycasts (which intersect the coarse `_data`) don't
  see, so it reads as **false cover** (you hide behind a bump and still get shot) or a **phantom occluder**
  (a shot that looks blocked lands). This is worst on **ridgelines/crests**, where even a small bump can
  just break or fake LOS. Mitigate with **modest amplitude** (few cm–~0.2 m, the water-wave discipline)
  and the fractal's object/flat/water gating (much of where units are). Escape hatch if it matters:
  conform the gameplay `SurfaceY`/terrain-intersect *queries* to the deterministic cached detail (fixes
  both float and sightlines) — but that re-couples gameplay + MP, so only if worth it. **Normals (Stage
  1) have neither problem** — they change shading only, never a silhouette or occluder, so they cannot
  create false cover or a phantom sightline. This is the decisive reason Stage 1 is the default and Stage
  2 is opt-in.
- **Exit:** near terrain has real relief with correct near-field shadowing; distance seamlessly falls back
  to Stage 1 normals.

### Stage 3 — Optional: baked normal map / tiling fallback
- Swap the in-shader detail-normal derivation for a **baked normal map** if the taps cost too much, or a
  tiling-detail approximation for oversized maps (§2). Pure optimization.

## 5. Open questions / risks
- **Detail texture size/format** vs quality — R16Float map-wide is the default; bound the amplitude to
  allow R8 if needed. Very large maps → the tiling fallback (§2).
- **Aliasing / mip matching** — sample the detail at a resolution matched to the current LOD (both VS
  displacement and FS normal), or high-frequency detail shimmers on coarse tiles.
- **Displacement pop** — the `morph_k` fade must bring displacement in *smoothly* as tiles refine, or
  detail visibly pops at LOD boundaries. Tie `displace_weight` to the same morph the geometry uses.
- **Visual/gameplay divergence (Stage 2, §4)** — not just cosmetic float/sink but **sightline
  disagreement** (false cover / phantom occluders, worst on ridgelines), since LOS/raycasts use the coarse
  `_data`. The decisive reason Stage 1 (normals — no silhouette/occluder change) is the default and Stage 2
  is opt-in.
- **Detail-normal space** — derive from the height gradient in the same space as the base normal so they
  add without seams.
- **Sun-shadow sweep stays coarse** — near detail relies on the prepass/SSAO for shadowing, not the sweep;
  fine if those exist, otherwise near detail is unshadowed (acceptable at cm scale; a reason to pair Stage
  2 with the AO work).

## 6. Cross-references
- [depth-prepass-plan.md](depth-prepass-plan.md) — the prepass + normal G-buffer that give Stage 2's near displacement its shadowing (SSAO/contact shadows).
- [terrain-conform-vegetation-roads-plan.md](terrain-conform-vegetation-roads-plan.md) — neighbouring terrain work (done); independent of this.
- [rendering-performance-plan.md](rendering-performance-plan.md) — why raising the whole grid (the `setTerrainGrid` route) is the expensive lever this plan avoids.
- [implementation-roadmap.md](implementation-roadmap.md) — where this sits (independent; Stage 2 pairs with Phase 2 SSAO).
