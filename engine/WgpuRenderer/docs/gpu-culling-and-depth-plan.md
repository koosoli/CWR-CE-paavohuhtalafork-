# Plan: GPU-driven rendering — retained scene, cull + LOD, indirect draw, Hi-Z occlusion

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** PLANNED (2026-07-08). Concrete.
**Roadmap slot:** Phase 4 — see [implementation-roadmap.md](implementation-roadmap.md).

> Moves the whole opaque object path onto the GPU: the level is a **GPU-resident retained scene**, and a
> compute pass does **distance + frustum + occlusion culling *and* LOD selection** per instance each
> frame, compacting survivors into **indirect draws**. The CPU stops walking objects per frame — it only
> streams *deltas* (spawns, moves, destruction) into the retained buffers. This is the "make an arbitrary
> view cheap" foundation the roadmap leans on: the same pass serves the main camera, the depth prepass,
> CSM cascades, and planar water reflections.
>
> **Owns:** the retained scene model, the cull+LOD compute, indirect draw, and Hi-Z occlusion.
> **Consumes:** the depth prepass ([depth-prepass-plan.md](depth-prepass-plan.md)) for Hi-Z; the
> destruction/keyframe morph and the per-instance record semantics from
> [gpu-object-rendering-plan.md](gpu-object-rendering-plan.md) (Stage 1, §5) — this plan realizes that
> record as the retained instance buffer rather than redefining it. Supersedes that plan's Stage 3
> (frustum cull + indirect), which now delegates here.

---

## 0. Where we are today (verified 2026-07-08)

**LOD + culling + submission is entirely CPU-side; the backend sees only post-cull, post-LOD sections.**

- **LOD container** `LODShapeWithShadow`: `_lods[32]` (one `Shape` per level) + parallel `_resolutions[32]`
  selection thresholds (sorted ascending; smaller = finer; ≥900 = special non-graphical levels), plus a
  bounding sphere/center/min-max ([Shape.hpp:687,737,910](../../Poseidon/Graphics/Rendering/Shape/Shape.hpp#L687)).
- **LOD selection** `Scene::LevelFromDistance2` ([SceneDraw.cpp:572](../../Poseidon/World/Scene/SceneDraw.cpp#L572)):
  `resol2 = distance² · (Camera::Left() · _lodInvWidth)²`, then `FindSqrtLevel(resol2)` picks the last
  level with `_resolutions[i]² ≤ resol2` ([ShapeLOD.cpp:1592](../../Poseidon/Graphics/Rendering/Shape/ShapeLOD.cpp#L1592)).
  Inputs: per-instance `distance²` (with a **near-clamp**: if `radius² > dist²·0.25`, use `(dist-radius)²`,
  [Scene.cpp:903](../../Poseidon/World/Scene/Scene.cpp#L903)); `Camera::Left()` (projection half-width);
  the global adaptive detail bias `_lodInvWidth` ([Scene.hpp:203](../../Poseidon/World/Scene/Scene.hpp#L203),
  driven by triangle-budget feedback [SceneDraw.cpp:966](../../Poseidon/World/Scene/SceneDraw.cpp#L966));
  draw distance `ENGINE_CONFIG.objectsZ` (default 600); a sub-pixel visibility cull.
- **Culling** two-tier: landscape grid-cell frustum cull ([World.cpp:1435](../../Poseidon/World/World.cpp#L1435))
  → per-object frustum (`Camera::IsClipped`, 6-plane sphere test, [Camera.cpp:211](../../Poseidon/World/Scene/Camera/Camera.cpp#L211))
  + distance + shadow-visibility in `Scene::ObjectForDrawing` ([Scene.cpp:794](../../Poseidon/World/Scene/Scene.cpp#L794)).
- **Submission**: LOD resolved to an int index CPU-side, then `Object::Draw(drawLOD)`
  ([Object.cpp:902](../../Poseidon/World/Scene/Object.cpp#L902)) → `Shape::Draw`
  ([ShapeDraw.cpp:80](../../Poseidon/Graphics/Rendering/Shape/ShapeDraw.cpp#L80), coalesces sections by
  material) → `EngineWgpu::DrawSectionTL` ([EngineWgpu.cpp:1372](../EngineWgpu.cpp#L1372)). The backend
  never sees the `LODShape` or any un-chosen level — one `WgrDraw3D` per coalesced material run.
- **Bounds** live on `LODShape` (`_boundingSphere/_boundingCenter/_minMax`), scaled per instance
  (`Object::GetRadius` [Object.hpp:560](../../Poseidon/World/Scene/Object.hpp#L560)); animated variants
  `AnimatedMinMax`/`AnimatedBSphere` ([Object.cpp:462](../../Poseidon/World/Scene/Object.cpp#L462)).

**Renderer side (Rust):**
- `WgrDraw3D` (264 B, [ffi.rs:130](../rust/src/ffi.rs#L130)): `mesh` handle, `index_begin/count`,
  `texture_id`, **camera-relative** `world` mat4, blend/sampler/depth/alpha_ref/flags, material + conform
  blocks, `palette_slot`. **No LOD field** — the renderer has no LOD concept.
- **One mesh handle = one `Shape` = one vbuf + one Uint16 ibuf; sections are index ranges** into that one
  buffer ([`Mesh` gfx3d/mod.rs:209](../rust/src/gfx3d/mod.rs#L209), `mesh_create`
  [:1311](../rust/src/gfx3d/mod.rs#L1311)). **Each LOD level is a *separate* mesh handle today**
  (each `Shape` → its own `wgr_mesh_create`), uploaded on demand.
- Per-draw storage arrays (perf Stage 1): `world: StorageArray<ObjectGpu>` (80 B) + `material:
  StorageArray<MaterialUbo>` (96 B), indexed by `@builtin(instance_index)`; **slot = base_instance =
  instance_index**, packed contiguously in slot order by `prepare`
  ([gfx3d/mod.rs:1936](../rust/src/gfx3d/mod.rs#L1936)). `draw_one` issues
  `draw_indexed(index_begin..index_end, 0, base..base+count)` ([:2331](../rust/src/gfx3d/mod.rs#L2331)).
- `plan_3d` ([:2118](../rust/src/gfx3d/mod.rs#L2118)) CPU-buckets instanceable opaque draws
  (`Opaque && offset==None && depth==TestWrite && !skinned`) by `{mesh, index range, texture, sampler,
  camera, pipeline}` → `Plan3dOp::Draw3D{draw, base, count}`. This is the compaction a GPU pass replaces.
- **Closest GPU-driven reference:** the shadow pass compacts casters into contiguous per-bucket instance
  ranges and replays them as instanced draws — but it is **CPU-built**, no compute, no indirect
  (`prepare_shadows`/`render_shadow_passes`, [:1562](../rust/src/gfx3d/mod.rs#L1562)).
- **No indirect draw anywhere.** No `MULTI_DRAW_INDIRECT`/`INDIRECT` in the crate. Optional features are
  adapter-gated and OR'd into `required_features` ([lib.rs:136-174](../rust/src/lib.rs#L136), the
  `partially_bound` pattern). Compute-pass templates: terrain shadow sweep
  ([terrain/mod.rs:401,794](../rust/src/terrain/mod.rs#L401)) and sky froxel
  ([sky/mod.rs:473,671](../rust/src/sky/mod.rs#L473)).
- **Camera UBO** (group 0) has `proj`, `view`, world `cam_pos` ([frame.wgsl:27](../rust/src/shaders/frame.wgsl#L27)),
  but **no frustum planes and no viewport size**. Geometry is camera-relative, so a distance is
  `length(world_pos.xyz)` (as the VS already does, [shader3d.wgsl:99](../rust/src/gfx3d/shader3d.wgsl#L99)).

---

## 1. The forcing constraints (why the data model must change)

Indirect multi-draw is the whole point — one `dispatch` culls, one `multi_draw_indexed_indirect` submits
— but it **cannot rebind vertex/index buffers, textures, or the pipeline between sub-draws**. Everything
in one indirect batch shares them. That dictates the retained model:

1. **Merged geometry pool.** Today each (model, LOD) is its own vbuf+ibuf. Indirect draws must all index
   **one shared vertex buffer + one shared index buffer**; each drawable section becomes a descriptor
   `{first_index, index_count, base_vertex}` into the pool. All resident geometry is suballocated there.
2. **Bindless object textures.** A per-draw `texture_id` bind is impossible under indirect. Objects move
   to a **`binding_array` of textures** (the exact infra terrain already uses — `TEXTURE_BINDING_ARRAY`
   is hard-required today), with a per-section **texture index** carried in the section descriptor /
   instance data and sampled non-uniformly in the fragment shader.
3. **Group by the few opaque pipeline variants.** One pipeline per indirect batch. The opaque set has a
   handful of variants (solid vs alpha-cutout; the rest — transparent, decal/ZBias, skinned — stay off
   the indirect path, §8). Emit one `multi_draw_indirect` per opaque variant. Cutout `alpha_ref` becomes
   per-instance/per-section data feeding a single cutout pipeline (aligns with the prepass foliage work).
4. **Absolute instance transforms, camera-relative in the shader.** The retained instance stores an
   **absolute** world transform (uploaded once); the VS subtracts `cam_pos` (group 0) — the model lights
   and terrain already use. This replaces today's per-frame CPU-computed camera-relative `world`.
5. **Derive frustum planes + add a viewport uniform.** The cull compute extracts 6 planes from
   `proj*view` (Gribb–Hartmann) and needs viewport size + the LOD constants in a new small cull-params
   uniform (neither exists today).

None of these are optional for GPU-driven indirect — they are the cost of admission, and all are
feasible on this low-poly content (the merged pool + all-resident geometry is a modest VRAM budget).

---

## 2. Retained scene model (the data layer)

A **unified** GPU instance store — static and dynamic objects are just "objects" to the GPU; the
static/dynamic split is CPU-side *update-cadence metadata*, not separate buffers (design decision).

### 2.1 Geometry pool
- One large **vertex buffer** + one **index buffer** (Uint32; the pool exceeds u16). At load, every
  resident `Shape` (see 2.2) is suballocated in, yielding a **section-descriptor** table entry per
  material run: `{first_index, index_count, base_vertex, texture_index, material_index,
  pipeline_variant}`. A free-list allocator handles the rare load/unload of geometry (map changes).
- **All LODs of all placed models + eager destroyed variants are resident after load** (§2.4). On this
  content the whole geometry working set fits; per-frame updates never touch geometry, only instances.
- **The destroyed variant is a parallel vertex stream, not a second mesh.** The generator only displaces
  the intact vertices — **same topology, order, and index buffer** — so a destructible section stores a
  *second position+normal stream* at the **same `base_vertex`**, sharing the intact section descriptor
  (same index range, texture, material). Destruction therefore adds **no section descriptors and no
  draws**; it's a per-vertex `mix` in the VS (§2.4). Only the position+normal stream is duplicated.

### 2.2 Model / LOD table
- Per model: an array of **drawable LOD levels**; per level: `resolution` (`_resolutions[i]`, the
  `FindSqrtLevel` threshold), a bounding sphere, and a **range into the section-descriptor table** (the
  sections of that level). This is exactly the input the GPU LOD select needs — the CPU `_resolutions[]`
  + bounds, uploaded once. Special (≥900) levels are excluded (never drawn).

### 2.3 Unified instance buffer
- Per instance (the retained record — the object plan's per-instance record §5, realized here):
  **absolute world transform** (or TRS), uniform `scale`, **model id** (indexes the LOD table),
  a single **`destroy_phase`** scalar (0 = intact, 1 = destroyed; no separate variant id — the destroyed
  stream shares the mesh's `base_vertex`, §2.1), material/palette overrides, and flags
  (on-surface/ZBias/skinned/static). Bounding data is the model's, scaled per instance.
- One **unified buffer** (`[static | dynamic]` regions); the static/dynamic distinction is only *how the
  CPU writes each region* (§2.4), invisible to the cull compute, which dispatches over the whole array.

### 2.4 Updating the instance buffer: static = patch, dynamic = re-copy
Two update cadences over the one buffer (design decision — simpler than a free-list for everything):
- **Static region — command patches, rare.** Uploaded at load; touched only when a static object actually
  changes (destruction, doors), via a coalesced `queue.write_buffer` (or a small "apply-patches" compute)
  over a short delta list. A quiet frame costs ~nothing.
- **Dynamic region — full re-copy every frame.** The whole dynamic set (vehicles, units, projectiles,
  effects) is re-written contiguously each frame. No free-list, no per-object dirty-tracking, no
  fragmentation for the churny set — and the CPU already walks these for simulation, so gathering them is
  nearly free. The cost is a sub-MB `queue.write_buffer` (a few hundred–thousand ~80–128 B records) —
  negligible, and it keeps the dynamic region always dense.
- *Fallback if GPU compaction (§3.3) proves fiddly:* since the CPU already handles the dynamic set, it can
  **CPU-drive dynamics** (cull + draw via the existing `plan_3d`) and **GPU-drive only the large static
  set** — removing GPU compaction for dynamics at the cost of two submission paths and no GPU occlusion
  for the (few) dynamic objects. A legitimate Stage-3 scope-reducer, not the default.

**Destruction = a per-instance linear morph, never an upload.** Each destructible model's destroyed vertex
stream is generated (`MakeDestroyed`) + uploaded **eagerly at load** as a parallel position+normal stream
sharing the intact topology/`base_vertex` (§2.1). A destruction event is then a pure state change — set
`destroy_phase` — and the VS **linearly interpolates**: `pos = mix(intactPos, destroyedPos, destroy_phase)`,
`normal = normalize(mix(nIntact, nDestroyed, destroy_phase))` (the generator guarantees matching topology
+ order, so the lerp is per-vertex with no remapping — [gpu-object-rendering-plan.md](gpu-object-rendering-plan.md)
Stage 1). **Cull bounds = union of intact + destroyed** so a mid-morph object never culls out. A *static*
object mid-destruction is command-patched each frame *while animating* (a handful of objects), then settles
into its destroyed state and goes quiet again — never a pool move, never a geometry upload.

### 2.5 Bindless object textures
- Objects adopt the terrain bindless pattern: a `binding_array<texture_2d>` + the shared sampler(s), with
  the section descriptor's `texture_index` selecting per fragment (non-uniform). Resolves the "can't
  rebind textures under indirect" constraint (constraint 2). Reuses the existing feature + upload path.

---

## 3. The cull + LOD compute pass

One compute dispatch over all instances (or over a coarse pre-cull list), per view.

### 3.1 Inputs
- The instance buffer (2.3), model/LOD table (2.2), section-descriptor table (2.1).
- Group-0 camera (`proj`, `view`, `cam_pos`) + a **new cull-params uniform**: viewport size,
  `objectsZ` (draw distance), `Camera::Left()` (proj half-width), `_lodInvWidth` (adaptive bias, §3.5),
  `pixelLimit`, near-clamp constant. The pass **derives 6 frustum planes** from `proj*view`
  (Gribb–Hartmann) once.

### 3.2 Per-instance test (replicating the CPU exactly)
1. **Distance**: `d² = |center − cam_pos|²`; near-clamp (`radius² > d²·0.25 → (d−radius)²`,
   [Scene.cpp:903](../../Poseidon/World/Scene/Scene.cpp#L903)). Cull if `d² > objectsZ²`.
2. **Frustum**: bounding sphere vs the 6 planes (matches `Camera::IsClipped`).
3. **Sub-pixel**: the same min-projected-size cull as `LevelFromDistance2`.
4. **LOD select**: `resol2 = d² · (Left·_lodInvWidth)²`; loop the model's `_resolutions[]` = `FindSqrtLevel`
   → chosen level. (Occlusion test, §5, slots in here for the color-pass cull.)

### 3.3 Compaction → indirect args (the genuinely hard part)
- A surviving instance at LOD *L* contributes one **(section, instance)** pair per section of *L*. Group
  by **section descriptor**: each pair atomically increments `draw_count[section]` and appends the
  instance slot to that section's **instance-index list** (a global list carved by per-section offsets —
  the classic two-pass "count then scatter", or a single atomic-append with a prefix-sum reserve).
- Build one **`DrawIndexedIndirect{index_count, instance_count, first_index, base_vertex,
  first_instance}`** per non-empty section, grouped into the per-pipeline-variant indirect buffers.
  `first_instance` = the section's base in the compacted instance-index list; `instance_count` =
  `draw_count`. Empty sections are skipped (compacted via an atomic draw counter, or a count buffer with
  `MULTI_DRAW_INDIRECT_COUNT`).
- **Output = the same layout the renderer already consumes**: a compacted instance-index list (→ the
  `world`/`material` slot each `instance_index` reads, §0) + indirect draw args. The VS is unchanged
  except it indexes the GPU-produced list instead of the CPU `order` array.

### 3.4 The draw
- Per opaque pipeline variant: bind group 0 (camera) + the bindless texture array + the geometry pool
  vbuf/ibuf + the instance/world/material buffers **once**, then one
  `multi_draw_indexed_indirect(args, count)`. Replaces the `plan_3d`→`draw_one` replay for the opaque set.

### 3.5 The `_lodInvWidth` feedback caveat
- `_lodInvWidth` is a CPU **triangle-budget feedback** loop ([SceneDraw.cpp:966](../../Poseidon/World/Scene/SceneDraw.cpp#L966));
  GPU-driven breaks the per-frame triangle read-back. Options (decide during impl): (a) keep a CPU
  estimate from last frame's culled counts; (b) **read back the GPU-emitted instance/triangle counts one
  frame late** and adapt (simplest correct closed loop, one-frame latency is invisible); (c) a fixed /
  view-distance-governed bias. Lean (b).

---

## 4. Indirect draw plumbing — portable, Metal must work (no Metal-specific code)

**Design principle: the GPU cull + compaction is identical on every backend; only the final *submission
call* degrades by a runtime feature check.** Metal does not expose `MULTI_DRAW_INDIRECT` through wgpu, and
we are **not** writing Metal-specific code now — so the whole design must run there, accepting a modestly
higher draw-call count. This is cheap to guarantee because of one fact:

- **Single `draw_indexed_indirect` is core wgpu and works on Metal**; only `multi_draw_indexed_indirect`
  (N sub-draws in one call) needs the `MULTI_DRAW_INDIRECT` feature. The compute writes the *same*
  `DrawIndexedIndirect` args buffer either way — the two paths differ only in how they're consumed:
  - **Feature present** (Vulkan/DX12): one `multi_draw_indexed_indirect(args, count)` per pipeline
    variant (with `MULTI_DRAW_INDIRECT_COUNT`, the GPU count buffer skips empty draws).
  - **Feature absent** (Metal): a **CPU loop of single `draw_indexed_indirect(args, offset_i)`** up to a
    conservative per-variant **cap**. Draw slots the compaction left with `instance_count == 0` are
    **no-op draws**, so the CPU need not know the live count — no read-back, no latency, no Metal code.
    The cost is extra draw-call submissions (fine — "slightly less efficient on Metal" is acceptable).
- Gate `Features::MULTI_DRAW_INDIRECT` (+ `INDIRECT_FIRST_INSTANCE` — `first_instance` must come from the
  args; else encode the instance-list base per-batch) + optional `MULTI_DRAW_INDIRECT_COUNT`, adapter-
  gated exactly like `partially_bound` ([lib.rs:159](../rust/src/lib.rs#L159)); add `BufferUsages::INDIRECT`
  to the args buffer. It is a **runtime branch**, not a `cfg!(metal)` — the same portable pattern already
  used for the optional bindless feature.
- **Last-resort fallback:** if an adapter lacks even the compute/storage prerequisites, fall back to the
  CPU `plan_3d` path entirely. That path stays as the correctness/A-B reference and the GL33-parity route
  regardless.
- **Derisk order:** first build a **CPU-produced indirect** path (replay today's `plan_3d` buckets via
  the same submission split above — proves the indirect plumbing, geometry pool, bindless textures, *and*
  the Metal loop on real hardware without the compute), *then* swap in the compute-produced args (§3).

## 5. Hi-Z occlusion (prepass-based)
- **Reduce the depth prepass into a Hi-Z pyramid.** The depth prepass is unconditional and lands first
  ([depth-prepass-plan.md](depth-prepass-plan.md), Phase 2); it is itself GPU-driven here but with
  **frustum + distance + LOD only, no occlusion** (§3 minus the Hi-Z test). Reduce its depth to a mip
  chain — **reversed-Z ⇒ `min` reduction** (keep the farthest/conservative depth; getting this backwards
  silently culls everything or nothing — the headline hazard). Under MSAA, reduce from the prepass's
  `min`-resolved single-sample depth (prepass plan §5).
- **Occlusion test (color-pass cull):** project each instance's bounds to screen, pick the Hi-Z mip whose
  texel footprint covers the projection, sample it, and cull if the bounds' nearest depth is behind the
  Hi-Z (conservative). This runs in the §3.2 test for the *color* pass only.
- **Why prepass-based, not two-phase reprojection:** we already pay for the prepass, so reusing it as the
  occluder set avoids the temporal disocclusion artifacts of the last-frame-reprojection scheme. (Two-
  phase remains the fallback if a scene's prepass-vs-color divergence ever matters.)

## 6. Multi-view (shadows + reflections)
- Parameterize the cull+LOD+indirect pass by an **arbitrary camera + optional clip plane**. The same
  machinery then serves: the **depth prepass** (main camera, no occlusion), the **color pass** (main
  camera + occlusion), **CSM cascades** (each cascade's frustum — replaces the CPU `prepare_shadows`
  compaction, [gfx3d/mod.rs:1562](../rust/src/gfx3d/mod.rs#L1562)), and **planar water reflections**
  (mirrored camera + waterline clip plane, [water-rendering-plan.md](water-rendering-plan.md) Stage 4b /
  roadmap Phase 5). Frustum+distance+LOD reuse per view trivially; occlusion is per-view (needs that
  view's own Hi-Z), so shadow/reflection views use frustum+distance+LOD only.

## 7. Staging (buildable increments)

Each stage ships and is measurable; the CPU path stays as the A/B reference until Stage 3.

- **Stage 1 — Retained data model (no compute yet).** Geometry pool (merged vbuf/ibuf + section
  descriptors), bindless object textures, unified instance buffer, model/LOD table, patch stream +
  free-list, eager destroyed variants. The **CPU still builds draws** by reading this model (via the
  existing `plan_3d`/`draw_one`), so it derisks the big data-model change independently of compute/indirect.
- **Stage 2 — CPU-built indirect.** Replay the `plan_3d` buckets as `multi_draw_indexed_indirect` over
  the pool. Proves indirect + pool + bindless with no compute. Feature-gated + CPU fallback.
- **Stage 3 — GPU cull + LOD compute → compute-built indirect (opaque set).** §3 in full: frustum +
  distance + LOD select + compaction. The CPU stops walking opaque objects. Occlusion **off** (frustum+
  distance+LOD only). This is the headline win.
- **Stage 4 — Hi-Z + occlusion.** §5: prepass → Hi-Z (`min`) → occlusion test in the color-pass cull.
- **Stage 5 — Multi-view.** §6: route CSM cascades and (Phase 5) reflections through the same pass.
- **Stage 6 — Skinned + transparent integration.** Fold skinned via the compute-skin-bake pool
  ([compute-skin-bake-plan.md](compute-skin-bake-plan.md)); keep transparents on the sorted CPU path or
  add a GPU sort. Tune, remove the CPU opaque path (keep the flag fallback).
  - **The bake (Phase 1) is already implemented + validated** (`WGR_SKIN_BAKE`, default off): it turns
    skinned meshes into rigid geometry in `skinned_vbuf`, `base_vertex`-addressed, drawn through the rigid
    pipelines with an identity world. **That rigid geometry is the entry point this stage consumes.** The
    bake is default-off because standalone it is pure overhead — VS skinning is ~free for OFP's low-poly
    characters, so amortizing it across passes saves nothing measurable (verified: shadow/prepass times
    are identical with the bake on/off). Its payoff is *here*: rigid baked geometry is what lets skinned
    soldiers be culled + indirect-drawn like static objects. Re-enable it as part of this stage.
  - **Do NOT build a standalone skin-bake "Phase 2" draw coalescer.** Phase 2's `multi_draw_indexed_indirect`
    coalescing IS a subset of this plan's indirect path — building it separately means building it twice.
    Instead, co-design two things here so the baked-output layout is defined ONCE against the geometry pool:
    1. **Batched compute dispatches.** Skinned VBs are shared per-model (`shape->GetVertexBuffer()`,
       [RtAnimation.cpp:1012](../../Poseidon/World/Simulation/Animation/RtAnimation.cpp#L1012)), so dozens of
       soldiers are a few meshes × many `palette_slot`s. Phase 1 dispatches once per slot; batch to one
       instanced dispatch per shared mesh (`instance_count > 1`) with a per-instance **palette-slot
       indirection table** (`block = slot_table[base + inst]`) — because slots are draw-order, not
       mesh-grouped — and **per-mesh-contiguous output** in `skinned_vbuf` so instances are addressable as
       `base + inst*vert_count`. That contiguity is exactly the geometry-pool layout, so define it together.
    2. **Draw coalescing = this plan's indirect draw.** Baked same-mesh soldiers become rigid instances in
       the pool and flow through §3/§4 indirect draws (per-instance `base_vertex` into the baked slice via
       the indirect args / a base in the instance list). This is where the real win lands: it collapses the
       per-soldier count-1 draws across CSM cascades + prepass + forward (draw-call-bound today) — not the
       skinning ALU.
  - **Cull skinned with a STATIC per-model bound, and cull BEFORE baking.** Skinned meshes need **no
    per-frame bound update** — verified: the CPU path never recomputes an animated object's bound
    (`AnimationRT::ApplyMatrices` only calls `InvalidateNormals()`,
    [RtAnimation.cpp:923](../../Poseidon/World/Simulation/Animation/RtAnimation.cpp#L923)); LOD + cull use the
    static `LODShape` sphere for standing and prone alike. So the §2.3 model bound (scaled per instance)
    IS the parity-correct skinned bound. It is also the only *possible* choice given ordering: cull must run
    **before** the bake (so only survivors are baked — no wasted skinning of culled soldiers), which means
    the cull bound cannot depend on baked vertices. A bind-pose bound isn't the full animation envelope, but
    that limitation is pre-existing (the CPU shares it) — if extreme poses ever pop, **inflate the static
    per-model bound by an envelope margin (a load-time constant), never recompute per frame.** If tight
    skinned bounds are ever truly needed (negligible payoff here), derive them from the ≤128 **palette**
    matrices in a pre-cull pass — deliberately NOT a reduction over baked verts, so cull→bake ordering holds.

## 8. Non-goals / fallbacks
- **Transparents** keep the CPU back-to-front path (indirect can't sort); a GPU sort is a later option.
- **Skinned** meshes need per-instance palettes — folded via compute-skin-bake (bake skinned verts to the
  pool once), then they instance like static geometry.
- **GL33 untouched.** Entirely wgpu-internal; the CPU `plan_3d` path remains the fallback when
  compute/indirect features are absent (or the flag is off).
- **Adaptive `_lodInvWidth`** closed loop (§3.5) — one-frame-late read-back, not a same-frame guarantee.

## 9. Load-bearing hazards
- **Reversed-Z Hi-Z reduction is `min`, not `max`** (§5). The headline correctness trap.
- **LOD parity** with `FindSqrtLevel`: replicate the near-clamp, sub-pixel cull, `Left·_lodInvWidth`
  factor, and ascending-`_resolutions` semantics exactly, or LODs pop differently than GL33 / the CPU path.
- **Indirect can't rebind** vbuf/ibuf/textures/pipeline (§1) — the merged pool + bindless textures +
  per-variant batching are mandatory, not optional.
- **`first_instance` from the indirect buffer** needs `INDIRECT_FIRST_INSTANCE`; without it, encode the
  instance-list base another way (per-batch offset uniform / a base in the instance list).
- **Instance-list / indirect-args overflow** — size for a worst case and `log()` truncation; never
  silently drop draws. The Metal single-`draw_indexed_indirect` loop caps at a fixed per-variant draw
  count (§4); size the cap for the worst case and `log()` if a frame would exceed it (excess sections
  would otherwise silently not draw on Metal only — a backend-divergent bug).
- **Destroyed-variant union bounds** (§2.4) — cull against `union(intact, destroyed)` or a morphing
  object culls out mid-destruction.
- **Camera-relative precision** — instances store absolute transforms; subtract `cam_pos` in the shader
  (as lights/terrain do) to keep large-map coordinates precise.
- **Reversed-Z / frustum-plane extraction** derived against the *actual* `proj*view` (shared with the
  Forward+ froxel slicing and water depth reconstruction — one helper).

## 10. Cross-references
- [implementation-roadmap.md](implementation-roadmap.md) — Phase 4; precedes planar reflections (Phase 5).
- [depth-prepass-plan.md](depth-prepass-plan.md) — the prepass this Hi-Z reduces (+ MSAA `min`-resolve).
- [gpu-object-rendering-plan.md](gpu-object-rendering-plan.md) — destruction/keyframe morph (Stage 1) + the per-instance record (§5) this plan realizes; its Stage 3 delegates here.
- [forward-plus-plan.md](forward-plus-plan.md) — shares the depth prepass and the reversed-Z hazards; clustered lighting shades the drawn set.
- [water-rendering-plan.md](water-rendering-plan.md) — planar reflections (Stage 4b) reuse the multi-view cull path + a clip plane.
- [compute-skin-bake-plan.md](compute-skin-bake-plan.md) — bakes skinned meshes into the pool so they instance (Stage 6).
- [rendering-performance-plan.md](rendering-performance-plan.md) — this is Stage 4 of the umbrella perf roadmap made concrete.
