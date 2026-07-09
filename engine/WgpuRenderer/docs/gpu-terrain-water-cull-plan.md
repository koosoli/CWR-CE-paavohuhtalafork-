# GPU-driven terrain & water culling + CDLOD selection

Status: **planning / analysis** (2026-07-09). No code yet. Sibling of
[gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) (objects) — this is the terrain/water
half of the same "make an arbitrary view cheap" multi-view cull story (that plan's §6).

Cross-refs: [water-cdlod-geometry-plan.md](water-cdlod-geometry-plan.md),
[cascaded-shadow-map-plan.md](cascaded-shadow-map-plan.md) (the N cascade views that dominate the ROI),
[depth-prepass-plan.md](depth-prepass-plan.md), [implementation-roadmap.md](implementation-roadmap.md).

## 0. Where we are today

Terrain and water share one **CPU** CDLOD driver ([`CdlodDriver.hpp`](../CdlodDriver.hpp) →
`SelectVisibleCdlod` → `SelectCdlod`/`EmitCdlodNode` in
[`TerrainCdlod.hpp`](../../Poseidon/World/Terrain/TerrainCdlod.hpp)):

- **Static quadtree, built once** (`BuildCdlodTree`): `CdlodNode { originX, originZ, size, minY, maxY,
  child[4] }`, `child[0] < 0` = leaf. `minY/maxY` are the world-height extent of each node, derived from
  the heightmap at load. Water uses an **over-sized** tree (reaches past the terrain to the horizon).
- **Per frame, per view (CPU):** clamp a visible world-xz rect, then recurse the tree — at each node
  `distSq` to its AABB vs `ranges[level]²` decides descend/emit, `camera.IsClipped(center, radius)` frustum-
  culls, an `extraVisible` predicate refines (water: reject nodes entirely above sea level, `minY > sea`).
  Emits `CdlodSelection { originX, originZ, size, level, morphStart, morphEnd }`.
- **Draw:** each selected patch becomes a `WgrTerrainNode`/`WgrWaterNode` (24 B: `{origin.xy, size, lod,
  morph_start, morph_end}`), the whole set is `write_buffer`'d into an instance buffer, and drawn as **one
  instanced `draw_indexed`** over a shared unit grid mesh (`node = instance`; the VS places/scales/morphs
  the grid and samples the heightmap texture). Terrain and water nodes are byte-identical.

So the **draw is already optimal** (one instanced call per view); the heightmap already lives on the GPU
(`wgr_terrain_set_heightmap`, R32Float). The only CPU-side per-frame work is the **selection traversal +
frustum tests + the node-buffer upload**.

## 1. Does it even make sense? (verdict: yes, but as multi-view work — not a standalone main-view opt)

**For the main view alone: marginal.** CDLOD's whole point is that the selected patch count is ~logarithmic
in draw distance, so 8 km costs barely more than 1 km — a few hundred patches, a traversal visiting a
low-thousands of nodes, a handful of `IsClipped` tests, and a few-KB upload. Tens of microseconds. Not a
measured bottleneck, and the draw is already one instanced call. Moving *just the main view* to the GPU
would trade that for compute-dispatch + readback/indirect complexity — poor ROI.

**The ROI is multi-view amplification.** The selection is redone *per view*, and the view count is about to
jump: main + depth prepass + **N shadow cascades** (CSM plan targets up to 8) + water **reflection**. That's
~10 CPU CDLOD traversals/frame, ×2 (terrain+water) for the ones that see both. A GPU cull pass parameterized
by `(frustum, ranges, rect, extraVisible)` does them all cheaply and in parallel, and it is the *same*
machinery the object cull needs for §6. Secondary wins: no per-frame CPU→GPU node upload, CPU freed for sim,
and one cull architecture across objects + terrain + water + shadows.

**Recommendation:** build it **with** the multi-view cull work (objects §6), not before. Optionally
prototype the wavefront on the main view first purely to de-risk the traversal, then generalize to N views.

## 2. Can CDLOD selection run entirely on the GPU? Yes.

The inputs are GPU-friendly: the tree is **static** (fixed nodes + precomputed `minY/maxY`) → upload once to
a storage buffer; `ranges[]`, `morphRegion`, the camera frustum/pos, the rect, and the `extraVisible` knob
are a small uniform. Output is written straight into the instance buffer (+ an indirect draw arg), so there
is no CPU traversal and no per-frame upload.

### 2.1 Approach: level-by-level **wavefront** (breadth-first), not per-node-independent

CDLOD is a recursive top-down descent whose correctness rests on a **2:1 LOD balance** (adjacent patches
differ by ≤ 1 level) enforced by the parent/child fallback in `SelectCdlod` (a child beyond its own range is
still emitted at its level when the parent descended — [TerrainCdlod.hpp:96-105]). A naïve "one thread per
node, decide with my own AABB distance" test can violate that at boundaries → **T-junction cracks**. So do a
faithful breadth-first traversal:

- **Queues** in a storage buffer: `queue_a`, `queue_b` (node indices) + atomic counters; `out_nodes` (the
  instance buffer) + an atomic count; a per-view indirect `DrawIndexedIndirect` arg.
- **Seed** `queue_a` with the root(s).
- **Per level** `L = numLevels-1 .. 0` (≈ 10–14 dispatches), one thread per node in the current queue:
  1. `distSq` = node-AABB-to-camera (reuse `CdlodNodeDistanceSq`). If `distSq > ranges[L]²` → the node is too
     far for this level; **don't emit here** — the parent already emitted it coarser (this is the recursion's
     `return false`, handled by the parent's fallback below).
  2. Frustum + rect + `extraVisible`. **If not visible → drop** (enqueue nothing: since child AABB ⊆ parent
     AABB, culling a node correctly culls its whole subtree — no ancestor bookkeeping needed).
  3. If leaf **or** `distSq > ranges[L-1]²` → **emit** `{origin,size,L,morphStart(L),morphEnd(L)}` (atomic-
     append to `out_nodes`); else **enqueue the 4 children** to `queue_b`, and for each child immediately
     re-test its own range: if the child would `return false` (child `distSq > ranges[L-1]²`) yet is visible,
     emit the child at `L-1` (the fallback). *(Equivalently: fold the fallback into the child pass — a child
     dequeued at `L-1` whose `distSq > ranges[L-1]²` emits itself rather than descending. Simpler; verify it
     reproduces `SelectCdlod` exactly.)*
  4. Swap `queue_a`/`queue_b`, reset the next counter (separate compute passes → wgpu auto-barriers between
     them, as terrain/sky computes already rely on).
- **Morph bands** come from `ranges[]` + `morphRegion` in the uniform (reproduce `CdlodMorphBand`/
  `EmitCdlodNode` exactly).
- **Draw:** `out_count` becomes the indirect arg's `instance_count`; one `draw_indexed_indirect` per view over
  `out_nodes`. Portable (core wgpu). No readback.

Cost: ≈ `numLevels` tiny dispatches per view; frontier width is bounded (CDLOD keeps it ~constant). Cheaper
than it looks, and it's the *per-view* cost that multi-view multiplies away from the CPU.

### 2.2 Multi-view generalization

Parameterize the compute by a per-view block `(view_proj frustum planes, cam_pos, ranges[], rect,
extraVisible flag)` and run it once per view, each writing its own `out_nodes` slice + indirect arg. This is
the terrain/water counterpart of the object cull's §6 multi-view: **one cull kernel serves main + prepass +
each CSM cascade + reflection.** Frustum-plane extraction reuses the object cull's `frustum_planes`
(camera-relative, row3 near — see the objects plan). Terrain and water share the kernel; water supplies its
own tree + the `belowSea` predicate (`minY <= seaThreshold`) + its over-sized rect.

## 3. Risks / open questions

- **Crack-free parity.** The wavefront must reproduce `SelectCdlod`'s fallback exactly, or coarse/fine seams
  crack. Mitigation: a golden test comparing GPU-selected vs CPU-`SelectVisibleCdlod` node sets for a set of
  camera poses (same tree/ranges) before trusting it in-game.
- **Whether the fallback can be flattened** into the per-child pass (2.1 step 3 variant) vs needing an
  explicit parent-emits-child path — settle by deriving against the recursion, not by eyeballing.
- **Water infinite extent** is already a finite over-sized tree, so no special-casing — but confirm the
  root/rect still bound the frontier (don't descend the whole ocean at grazing angles; the distance ranges +
  rect already do this on CPU).
- **minY/maxY upload** — computed once at load (heightmap-derived). If terrain deforms at runtime (craters?)
  the affected nodes' bounds go stale; today the CPU tree has the same property, so no regression, but note it.
- **Indirect-draw availability** — `draw_indexed_indirect` is core wgpu; the count-trim/`multi_draw_*_count`
  fast path (objects 3b-4) isn't needed here (one draw per view, `instance_count` from the compute).
- **Sequencing dependency** — only worth building once the multi-view consumers (prepass/CSM/reflection)
  exist and each does its own selection; before that it's a main-view-only marginal win.

## 4. Staging

1. **(Prereq)** Multi-view cull machinery from objects §6 exists (or is co-developed).
2. **Upload the static CDLOD tree** (nodes + `minY/maxY`) + ranges/morph to GPU buffers (once; terrain & water).
3. **Wavefront selection compute** (single main-view first, to de-risk) writing `out_nodes` + an indirect
   arg; swap the CPU `SubmitTerrain`/`SubmitWater` upload for it. Golden-test vs CPU selection.
4. **Indirect instanced draw** consuming the compute output (terrain, then water with `belowSea`).
5. **Generalize to N views** (prepass + CSM cascades + reflection): per-view param block, per-view output
   slices + indirect args. This is where the CPU cost actually collapses.
