# Plan: Forward+ clustered lighting (froxel light culling)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** PLANNED (2026-07-08). Concrete. This is the `Forward+` roadmap item (HDR → per-pixel → CSM →
**Forward+**).
**Roadmap slot:** Phase 2, **parallel track** — see [implementation-roadmap.md](implementation-roadmap.md).

> **Ownership split (changed 2026-07-08).** The depth prepass this effort once contained is now its own
> plan — [depth-prepass-plan.md](depth-prepass-plan.md) — because water and occlusion culling consume it
> too. **This plan owns only the clustered lighting.** It *uses* the prepass for overdraw reduction and
> optional active-cluster culling, but the **cluster grid itself is prepass-independent**, so Forward+
> does not block on it and is not a blocker for water (which needs only the prepass).
>
> **Why forward (and why this is worth it): MSAA.** Staying forward-shaded — rather than deferred — is a
> deliberate choice so the renderer can enable **MSAA** and **alpha-to-coverage** later (§6). Clustered
> forward lighting is MSAA-native (per-pixel shading, hardware edge AA); a deferred G-buffer makes MSAA
> expensive/awkward. Forward+ is what makes "many lights" affordable *without* giving up that path.

---

## 1. Motivation

Lighting today is a **flat frame-global loop**: up to `MAX_LIGHTS = 256` point/spot lights are uploaded
once into a group-0 storage buffer ([gfx3d/mod.rs:106-108](../rust/src/gfx3d/mod.rs#L106),
[upload :443-446](../rust/src/gfx3d/mod.rs#L443)) and **every fragment iterates every active light** via
the shared `lighting::lights_contrib` ([lighting.wgsl:41-94](../rust/src/shaders/lighting.wgsl#L41)),
used by the object shader (`fs_main`), the terrain shader (`fs_terrain`), and — after Phase 3 — water.
That is `O(pixels × lights)`: fine for a few lamps, quadratic pain for a night town full of street
lights, and it is paid on objects **and** terrain **and** water. Forward+ replaces the per-fragment
global loop with a **per-cluster light list**: cull lights into view-frustum froxels once per frame, then
each fragment tests only the handful of lights touching its cluster. The `WgrLight` buffer stays the
light-data source; Forward+ adds cluster index structures on top.

## 2. Verified starting facts (2026-07-08)

- **Flat 256-light loop** (above). `WgrLight` record: [ffi.rs:382](../rust/src/ffi.rs#L382) /
  [gfx3d/mod.rs:394](../rust/src/gfx3d/mod.rs#L394); bound as group(0) `@binding(3)`
  ([frame.wgsl:56](../rust/src/shaders/frame.wgsl#L56)). Active count rides in `frame.cam_pos.w`.
- **Shared shading entry.** `lights_contrib(world_rel, normal, mat_diffuse, mat_ambient, linear)` is the
  one place all lit pipelines loop lights — change it once, objects + terrain + water all cluster.
- **Reversed-Z** (near = 1, far = 0). The z→slice mapping must be derived against it (§5, headline hazard).
- **An aerial-perspective froxel volume already exists but is a *different* thing.** `cs_froxel` in
  `sky.wgsl` fills a 3D volume of fog inscatter/transmittance ([frame.wgsl:82](../rust/src/shaders/frame.wgsl#L82)).
  It is **not** the light-cluster grid — but it is the same idea (a froxel grid over the view frustum with
  an exponential depth distribution), so **share the reversed-Z depth→slice helper and, ideally, the grid
  dimensions** to avoid two divergent froxel mappings.
- **No new device features expected.** Storage buffers in the fragment stage and atomics are core wgpu;
  confirm against the adapter. Group(0) currently uses bindings 0–8 — clustering needs either more
  bindings or a dedicated bind group (§5; raising `max_bind_groups` ≥ 8 is fine on our desktop target).
- **No MSAA today** (`MultisampleState::default()`, count 1 everywhere) — but the whole point is to keep
  MSAA reachable (§6).

## 3. Design decisions

1. **Clustered (3D froxels), not tiled.** Tiles × depth slices, so lights are bounded in Z as well as
   screen space (a tiled 2D grid over-includes lights along the view ray). The grid tiles the **whole
   frustum**, independent of scene depth — so it needs no prepass to be correct (decision 2). Start at
   ~**16×16 or 32×32 px tiles** and **24–32 exponential depth slices**; e.g. 1920×1080 @ 32 px × 24
   slices ≈ 60×34×24 ≈ 49K clusters (tune down with coarser tiles if the cull pass is heavy).
2. **Cluster grid is prepass-independent; the prepass only *optimizes*.** Correctness needs no depth
   prepass. Use [depth-prepass-plan.md](depth-prepass-plan.md) for two wins: (a) **overdraw reduction** —
   the per-cluster light loop then runs ~once per visible pixel, not once per overdrawn fragment; (b)
   optional **active-cluster culling** — mark clusters that contain geometry (from the prepass depth
   min/max per tile) and skip empty ones in the cull pass. Neither is required to ship Stage 3.
3. **Precompute cluster AABBs on resize/FOV change only.** A compute pass builds each cluster's
   view-space AABB from its froxel bounds (screen tile → frustum corners at the slice's near/far view-z),
   stored in a buffer, dirty-flagged — not per frame.
4. **Light-culling compute pass, two-buffer layout.** Per cluster, test each `WgrLight`'s bounding volume
   (sphere for point, cone for spot) against the cluster AABB; append surviving light indices into a
   global **light-index list** via an atomic offset, and write the cluster's **{offset, count}** into a
   **cluster-grid buffer**. Size the index list for a worst-case average lights/cluster and **`log()` if
   truncated — never silently overflow**. One workgroup per cluster (or per tile looping its slices).
5. **Shade through the shared `lights_contrib`.** The fragment reconstructs its cluster from
   `frag_coord.xy` (tile) + view-space depth (slice, reversed-Z mapping), reads `{offset, count}`, and
   loops only that cluster's slice of the light-index list out of the existing `WgrLight` buffer.
   Because objects, terrain, and water all call `lights_contrib`, one edit clusters all three. **Keep the
   flat loop behind a shader-def flag** (`clustered` on/off) as the A/B correctness reference and adapter
   fallback.
6. **Per-pixel shading (MSAA-native).** Shade once per pixel; MSAA supplies geometry-edge AA via
   coverage, and alpha-to-coverage foliage composes without special handling (§6). The cluster lookup
   uses the pixel-centre depth. This is the crux of the forward-vs-deferred choice — see §6.
7. **Bindings.** Add the `cluster_grid` + `light_index_list` (+ `cluster_aabb` if kept) storage buffers.
   Prefer a **dedicated "clustering" bind group** bound by the object/terrain/water pipelines over
   stuffing more into group(0); decide during impl against the bind-group budget.
8. **GL33 untouched; wgpu-only, flagged (`WGR_FORWARD_PLUS`).** Flat loop stays selectable.

## 4. Stages

### Stage 1 — Cluster grid + AABBs
- Grid-dimensions UBO (tiles × slices, reversed-Z slice distribution); AABB precompute compute pass;
  recompute on resize/FOV. Debug viz: tint fragments by cluster index.
- **Exit:** clusters map correctly across the frustum; the reversed-Z slice mapping is unit-tested.

### Stage 2 — Light-culling compute
- Per-cluster sphere/cone-vs-AABB test → light-index list + cluster-grid `{offset,count}`. Debug
  overlay: heatmap of lights-per-cluster; overflow counter logged.
- **Exit:** per-cluster light lists match a brute-force reference for a known light set.

### Stage 3 — Shading integration (objects + terrain), behind the flag
- `lights_contrib` reads the cluster list; `clustered` shader-def gates it against the flat loop. A/B
  must be pixel-identical where light counts are low.
- **Exit:** night-town scenes shade at a fraction of the flat-loop cost; identical look to the fallback.

### Stage 4 — Optimizations + reach
- Prepass-assisted active-cluster culling (decision 2b); tile/slice tuning; extend the clustered path to
  water ([water-rendering-plan.md](water-rendering-plan.md), Phase 3+). Optional: share the grid with the
  aerial froxel.
- **Exit:** clustering covers all lit surfaces; grid tuned; empty clusters skipped.

## 5. Load-bearing hazards
- **Reversed-Z depth→slice mapping (headline).** A slice function written for conventional depth bins
  froxels backwards — lights land in the wrong clusters and lighting goes subtly-to-grossly wrong with no
  crash. Derive `view_z ↔ slice` against the actual reversed-Z projection and **unit-test it**. Same
  reversed-Z family as the Hi-Z `min` hazard ([gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md))
  and water's depth reconstruction — share one helper.
- **Light-index list overflow.** Cap + `log()` the truncation; never silently drop lights.
- **Depth used for slice reconstruction** must be consistent view-space linear depth (from the reversed-Z
  buffer or recomputed from the fragment position), matching the cull pass's slice bounds.
- **Bind-group budget** (decision 7).

## 6. MSAA & alpha-to-coverage — why this plan stays forward

MSAA is a **primary intended capability**, and it is the reason lighting is being scaled with *forward*
clustering rather than a deferred G-buffer:

- **Forward clustered shading is MSAA-native.** Enabling MSAA is a `sample_count` change on the colour
  (and depth-prepass) pipelines; shading stays **per-pixel** (decision 6), the raster supplies edge
  coverage, and the cluster lookup is unaffected. A deferred renderer would need a **multisampled
  G-buffer** and per-sample lighting resolves at edges — expensive in bandwidth and complexity, and the
  main reason deferred engines often drop MSAA for post-AA. Forward+ keeps MSAA cheap.
- **Alpha-to-coverage foliage composes for free.** Under MSAA, cutout foliage uses
  `alpha_to_coverage_enabled` (+ derivative alpha rescaling) for AA'd, distance-stable silhouettes, run
  **identically in the depth+normal prepass and the colour pass** so coverage matches and foliage lands
  in the G-buffer (see [depth-prepass-plan.md](depth-prepass-plan.md) decisions 3 & 10). Clustered
  per-pixel shading needs no change for this.
- **HDR resolve.** When MSAA lands the `Rgba16Float` scene target is multisampled and resolves to
  single-sample HDR before tonemap/bloom/exposure (a colour `resolve_target`, supported by WebGPU). This
  is a renderer-wide switch, tracked in the prepass plan §5; Forward+ imposes no extra MSAA cost beyond it.

## 7. Cross-references
- [implementation-roadmap.md](implementation-roadmap.md) — Phase 2 parallel track; not a water blocker.
- [depth-prepass-plan.md](depth-prepass-plan.md) — consumed for overdraw reduction / active-cluster culling; shares the reversed-Z slice/depth helpers and the MSAA story.
- [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) — shares the depth prepass and the reversed-Z hazards.
- [water-rendering-plan.md](water-rendering-plan.md) — water's `lights_contrib` gets clustering for free (Stage 4).
- [rendering-performance-plan.md](rendering-performance-plan.md) — this is the lighting-scalability piece of the umbrella perf roadmap.
