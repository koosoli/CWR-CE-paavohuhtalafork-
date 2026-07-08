# Plan: wgpu renderer performance (batching / instancing / GPU-driven)

**Status:** Stage 0/1 in progress (2026-07-05). The wgpu backend renders every
mesh section as its own `draw_indexed(count=1)` and uploads per-draw data with one
`queue.write_buffer` per draw, so a typical scene issues ~44K Vulkan calls, ~16K of
them frame-start buffer-copy + barrier pairs. This plan removes that overhead in
independently-shippable stages, keeping Metal viable (no multi-draw-indirect
required before the optional final stage).

> Cross-plan sequencing (how this umbrella's Stage 4 GPU-driven/indirect work relates to Forward+, GPU
> culling, water, and planar reflections) lives in [implementation-roadmap.md](implementation-roadmap.md).

## Problem

The backend is a pure draw-recorder. The engine's classic scene traversal (the
same `Object::Draw` / section path GL33 uses) calls `EngineWgpu::DrawSectionTL`
once per mesh section per object; each call unconditionally does:

```cpp
_draws3d.push_back(d);                            // EngineWgpu.cpp
_cmds.push_back(WgrCmd{WGR_CMD_DRAW_3D, ...});    // one command per section
```

No dedup, no sort, no instance detection. `Renderer::render_frame` replays the
command stream one-for-one. Two storms result, matching the RenderDoc capture:

1. **Frame-start upload storm (~16K copies+barriers).** `Gfx3d::prepare` loops the
   draw list writing the per-draw world matrix (64 B) and material (96 B) with a
   *separate* `queue.write_buffer` each. Every `write_buffer` becomes its own
   staging `vkCmdCopyBuffer` + barrier, so `N` draws → `2N` copies. Plus per-slot
   palette writes and per-frame CPU-skinning `mesh_update` re-uploads.

2. **Per-draw bind storm.** `Gfx3d::draw_one` issues, for every section:
   `set_pipeline` + 4× `set_bind_group` + 1–2 `set_vertex_buffer` +
   `set_index_buffer` + `draw_indexed(range, 0, 0..1)`. No redundant-state
   elimination; instance count is always 1.

**Culling** already works — but it is CPU, upstream, in the shared engine
traversal; the backend only sees post-cull, post-LOD sections and has no culler of
its own. It is *not* the current bottleneck. The terrain is the one subsystem that
already batches correctly (`WgrTerrainNode` uploaded as instance-step vertex data,
one instanced draw) and is the template for the rest.

## Staging

Ordered by ROI-per-risk. Each stage is measurable and shippable on its own.

### Stage 0 — Instrumentation (debug groups + counters)
- RenderDoc debug groups (`push_debug_group`/`pop_debug_group`) around every render
  phase: shadow cascades, terrain shadow-mask sweep, each colour segment, overlay.
- Buffers keep descriptive labels so coalesced uploads are identifiable by
  destination in the capture.
- Per-frame counters: draws, `write_buffer` calls, bind-group sets. Optional GPU
  timestamp queries around the segment pass for before/after numbers.

### Stage 1 — Coalesce uploads + draw-indexed storage buffers (kills storm #1)
- Replace the per-draw `write_buffer` loops with **one packed CPU buffer written in
  a single `write_buffer`** per array (world, material, palette, camera). `2N+`
  copies → a handful.
- Move per-draw world + material from **dynamic-offset UBOs to storage buffers
  indexed by `@builtin(instance_index)`**, passing the draw's slot as the
  instanced-draw `base_instance`: `draw_indexed(range, 0, slot..slot+1)`. This
  removes the group(1) dynamic-offset rebind entirely — group(1) is bound **once
  per frame** — and is the exact layout Stage 3 instancing needs (a run of
  instances becomes `slot..slot+N`).
- Skinned draws: fold their material into the same storage buffer (indexed by
  `instance_index`); keep the bone palette as a per-skinned-draw dynamic UBO for
  now (characters are a minority; compute-skin-bake handles them at Stage 4).
- `max_storage_buffers_per_shader_stage` / `max_storage_buffer_binding_size` are
  ample on the desktop target; storage buffers in the vertex stage are core wgpu.

### Stage 1b — Per-frame vertex re-upload storm (`wgr_3d_vbuf`)
The dominant frame-start copy storm turned out to be vertex re-uploads, not
world/material. `VertexBufferWgpu::Update` re-uploads whenever `isDynamic ||
dynamic || bufferDirty`; `isDynamic` covers every `GetAllowAnimation` shape, and
terrain-draped objects (`ClipLandKeep`/`ClipLandOn`) call `InvalidateBuffer()`
every frame in `Object::Animate`/`Deanimate` even though their geometry is
terrain-static.

- **Done:** content-hash skip in `Update` (FNV-1a over the rebuilt verts; skip
  `wgr_mesh_update` when byte-identical to the last upload). Eliminated the *road /
  single-instance* re-uploads: a distinct draped object is terrain-static, so its
  bytes match across frames → uploaded once.
- **Open — vegetation / instanced shapes.** The hash only remembers the *last*
  upload, so it is defeated when **many instances share one shape+buffer** and each
  instance writes different data through it within a frame. In RenderDoc: the same
  `wgr_3d_vbuf` region is overwritten dozens–hundreds of times in a row, each
  followed by one draw. The per-instance difference is terrain draping (the engine's
  wind — `Landscape::GetWind` — drives cloth/flags, not tree/bush vertex sway, so
  vegetation deforms via `ClipLand*`, same path as roads but ×N instances). This is
  also **latently incorrect** in the deferred renderer: all the `queue.write_buffer`
  writes flush before the render pass, so every instance's draw samples the *last*
  instance's geometry.
  **Fix (later pass, agreed): move per-instance deformation to the GPU.** Upload the
  base (undeformed) mesh once; drape/animate per instance in the vertex shader using
  the per-instance world matrix (already in the Stage-1 storage buffer) + a terrain
  height sample (the heightmap is already GPU-resident in the terrain system) and/or
  a wind uniform. Vegetation then becomes a static mesh + instanced draws — folds
  directly into Stage 3 instancing and removes the storm and the correctness bug at
  once. Retain the CPU path for GL33.

### Stage 1c — Shadow-cascade caster over-draw (observed)
Casters are emitted with `cascade_mask = 0xF` and drawn in every cascade regardless
of overlap, so one caster costs `count` draws; a single vegetation piece was seen
rendered ~8× across the 4 cascades (also suggesting per-section duplication). Cheap
win independent of the above: set each caster's `cascade_mask` from the cascades its
bounds actually touch, and merge/paletting per-section casters.

### Stage 2 — State-sorted replay + redundant-bind elimination (cuts storm #2)
- Sort *opaque* 3D draws within each depth-segment by
  `(pipeline, mesh, texture, sampler)`; keep transparent draws in submission order
  (back-to-front) and preserve the 2D/terrain interleave and `CLEAR_DEPTH` segment
  boundaries.
- `draw_one` tracks currently-bound pipeline/buffers/binds and skips unchanged
  `set_*` calls. Big cut with no instancing and no shader change.

### Stage 3 — Instancing (collapses the draw count)
- Bucket draws by `(mesh, index_range, pipeline, texture, sampler)` across the
  frame; emit one `draw_indexed(range, 0, base..base+N)` per bucket, per-instance
  world+material read from the Stage-1 storage buffers via `instance_index`.
  Forests, bushes, fences, repeated props, identical soldiers collapse to one draw
  each. Metal-safe (plain instanced draw). Opaque pass first; transparent stays on
  the Stage-2 path.

### Stage 4 — GPU-driven culling + LOD + indirect (concrete plan)
- **Bindless object textures + samplers pulled forward** (2026-07-08, implemented, uncommitted) — see
  [bindless-textures-plan.md](bindless-textures-plan.md). One `binding_array<texture_2d>` + a
  `binding_array<sampler,8>` bound once for the lit-mesh + prepass; texture/sampler indices ride the
  per-instance material, so they drop out of the instancing key (bigger batches) and off the per-draw bind
  path. Cuts the per-draw fixed cost the [depth prepass](depth-prepass-plan.md) measured as its dominant
  overhead, for both the colour and prepass passes. The rest of Stage 4 below still pending.
- Designed in full in [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md): a GPU-resident
  retained scene (merged geometry pool + bindless object textures + unified instance buffer), a compute
  pass doing distance + frustum + occlusion culling **and LOD selection**, compacted into indirect draws.
  `multi_draw_indexed_indirect` where supported; a portable single-`draw_indexed_indirect` loop on
  **Metal** (no Metal-specific code); CPU `plan_3d` path retained as the fallback + GL33 route. Lands the
  deferred compute-skin-bake work ([compute-skin-bake-plan.md](compute-skin-bake-plan.md)) so skinned
  meshes bake into the pool and instance too.

## Expected outcome
Stage 1 alone removes roughly half the API calls (the upload storm) for little
risk. Stages 2–3 together should bring a typical scene from ~44K calls into the low
thousands. Stage 4 is reserved for when CPU cull traversal itself becomes the limit.
