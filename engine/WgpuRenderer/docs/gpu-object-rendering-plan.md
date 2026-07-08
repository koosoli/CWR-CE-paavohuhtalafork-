# Plan: GPU-native object rendering for the wgpu renderer (destruction morph, decal instancing, GPU culling)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Engine:** Poseidon (Operation Flashpoint / Arma: Cold War Assault lineage), C++
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** proposed (2026-07-06). Supersedes the earlier "move roads/objects off software T&L"
draft, whose Stage A/B were built on stale premises (see §2).

> Handoff brief for concrete work. Line numbers are **approximate** — identifiers are the
> source of truth. Every §3 claim here was cross-checked against the current branch; where a
> prior draft's claim was wrong, this doc says so and cites the code.

---

## 1. Objective

Move the remaining *genuinely* CPU-bound object render work onto the GPU for the **wgpu path
only**, extending the retained-world direction the terrain and conform work already established.
Three things are actually unbuilt and worth doing, in priority order:

1. **Destruction / keyframe animation as a vertex-shader morph** between two static meshes —
   required (not merely faster) before destructible objects can be instanced.
2. **Instancing the decal / on-surface path** (roads, paths) — the one object class the existing
   color-path instancer deliberately excludes today.
3. **GPU culling + indirect draw** — the only part that stops the CPU from iterating objects
   per frame.

**Hard constraint: no negative impact on GL33.** Every change is either wgpu-backend-internal
(Rust, invisible to the engine) or gated the same way `GGpuTerrainConform` already gates the
conform path — GL33 keeps its existing CPU code unchanged and stays the default. See §6.

## 2. Relationship to existing plans (read these first)

This plan does **not** re-plan work already scoped or done:

- [implementation-roadmap.md](implementation-roadmap.md) is the cross-plan phase order. This plan's
  Stage 3 (frustum cull + indirect) is the Phase-4 **multi-view foundation** that planar water
  reflections (Phase 5) consume — design the cull pass around an arbitrary camera + clip plane (Stage 3).
- [terrain-conform-vegetation-roads-plan.md](terrain-conform-vegetation-roads-plan.md) already
  owns the conform story. Vegetation (`ForestPlain` plane + `ClipLand` per-vertex heightmap
  conform) is **implemented and user-verified** for color and shadow passes. Roads are scoped
  there as **Part B** (B1: memoize `SurfaceSplit`; B2: bake a static VB at load). *Nothing in
  the present plan re-does that;* Stage 2 here builds on top of it (instancing the result).
- [rendering-performance-plan.md](rendering-performance-plan.md) is the umbrella perf plan;
  its Stage 2 (sort) / Stage 3 (instancing) are the buckets Stages 2–3 here feed into.
- [compute-skin-bake-plan.md](compute-skin-bake-plan.md) owns the skinning-on-GPU direction;
  the per-instance palette-offset field (§5) is shared with it.

**Corrections carried over from the superseded draft (verified in code):**

- Roads have **no vertex buffer at all** — `OnSurface` levels disable VB conversion in
  `LODShape::OptimizeRendering` ([ShapeDraw.cpp:351-354](../../Poseidon/Graphics/Rendering/Shape/ShapeDraw.cpp#L351-L354)).
  The per-frame road cost is a CPU `SurfaceSplit` rebuild into a transient `tlTable`, not a VB
  re-upload. "Roads dirty their VB every frame" was false.
- Per-frame conform regeneration is **already gated out** on the wgpu path: with a conform
  plane active, `Object::Animate` skips the deform + `InvalidateBuffer`
  ([Object.cpp:387](../../Poseidon/World/Scene/Object.cpp#L387)), `Object::Deanimate` skips the
  restore ([Object.cpp:552](../../Poseidon/World/Scene/Object.cpp#L552)), and bounds are cached
  in `Object::AnimatedMinMax` ([Object.cpp:462-508](../../Poseidon/World/Scene/Object.cpp#L462-L508)).
  The draft's "Stage A" was therefore mostly already done.
- The color path **already instances**. See §3.

## 3. Verified current state

**Color-path instancing exists.** `plan_3d` coalesces consecutive standard-opaque draws into
instanced draws over a contiguous `base..base+count` slot range
([gfx3d/mod.rs:2071-2164](../rust/src/gfx3d/mod.rs#L2071-L2164); the draw call is
`pass.draw_indexed(index_range, 0, base..base+count)` at
[mod.rs:2281](../rust/src/gfx3d/mod.rs#L2281)). The instanceable predicate
([mod.rs:2120-2123](../rust/src/gfx3d/mod.rs#L2120-L2123)) is:
`!skinned && blend == Opaque && offset == Offset::None && depth == TestWrite`. Everything else
(transparent, **decal**, ZBias, skinned, non-standard depth) is a **barrier** draw emitted
count-1 in place, preserving order. So the SSBO/`base_instance` machinery the draft described as
"laid out for a future Stage 3" is **already feeding live instanced draws today.**

**Per-instance SSBO (`ObjectGpu`).** `struct ObjectGpu { world: mat4; conform0/1/2: vec4 }` —
112 B, three conform vec4s, not one plane ([mod.rs:114-121](../rust/src/gfx3d/mod.rs#L114-L121)).
Held in a growable `StorageArray` ([mod.rs:616-647](../rust/src/gfx3d/mod.rs#L616-L647), power-of-two
growth, one `write_buffer` per frame), indexed in-shader by `@builtin(instance_index)` ==
`base_instance`. A parallel `material: StorageArray<MaterialUbo>` exists alongside it.

**`PipelineKey`** = `{ blend, depth, offset, alpha_ref_bits, skinned }`
([mod.rs:37-44](../rust/src/gfx3d/mod.rs#L37-L44)); `offset` ∈
`{None, Decal, ZBias(1..3), Shadow}` ([mod.rs:26-31](../rust/src/gfx3d/mod.rs#L26-L31),
`from_draw` [mod.rs:76-94](../rust/src/gfx3d/mod.rs#L76-L94)). Roads take **`Offset::Decal`**
(via `WGR_DRAW3D_ON_SURFACE`), which is why they are excluded from instancing. Batch identity
`BucketKey` also folds in mesh/index-range/texture/sampler/camera
([mod.rs:2289-2297](../rust/src/gfx3d/mod.rs#L2289-L2297)).

**Shadow path already coalesces** casters into one instanced draw per bucket **per cascade**
(`prepare_shadows` [mod.rs:1517-1642](../rust/src/gfx3d/mod.rs#L1517-L1642), draws at
[mod.rs:1644-1733](../rust/src/gfx3d/mod.rs#L1644-L1733); skinned casters can't instance and go
count-1). This is the working model to mirror.

**Indirect draw is not used.** No `MULTI_DRAW_INDIRECT` / `draw_indexed_indirect` anywhere. All
draws are direct. `required_features` requests BC compression + bindless
(`TEXTURE_BINDING_ARRAY | SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING`, plus
optional `PARTIALLY_BOUND_BINDING_ARRAY`) — [lib.rs:93-114](../rust/src/lib.rs#L93-L114);
`max_bind_groups = 5`.

**Destruction is a per-vertex CPU morph today.** `Object::Animate`
([Object.cpp:323](../../Poseidon/World/Scene/Object.cpp#L323)) lerps every vertex of the
**shared** level shape toward a cached destroyed variant:
`pos = dPos*ratio + pos*(1-ratio)` ([Object.cpp:350-355](../../Poseidon/World/Scene/Object.cpp#L350-L355)),
`ratio = GetDestroyed()` (`_destroyPhase/255`), snapping to the destroyed mesh at `ratio > 0.99`
([Object.cpp:341-347](../../Poseidon/World/Scene/Object.cpp#L341-L347)), then
`InvalidateBuffer()`/`InvalidateNormals()` ([Object.cpp:357-358](../../Poseidon/World/Scene/Object.cpp#L357-L358)).
The destroyed target is generated **once per model** with a seeded RNG and cached on the shared
`LODShapeWithShadow::_destroyed`, per LOD (`MakeDestroyed`/`MakeTreeDestroyed`,
[ObjTrash.cpp:16,71,104](../../Poseidon/World/Scene/ObjTrash.cpp#L71)). It only works today because
the shared shape is mutated in immediate mode and restored by `Object::Deanimate`
([Object.cpp:527-541](../../Poseidon/World/Scene/Object.cpp#L527-L541)) — `SaveOriginalPos` /
`RestoreOriginalPos` around each draw. **Two instances of one model at different collapse phases
would fight over the same bytes**, so a retained/instanced VB *cannot* use the CPU morph. The
same is true of RTM keyframe interpolation (`Shape::SetPhase` / `PointPhase`, a blend between two
static poses).

Base `Object::Animate` has exactly **two** branches: the destruction lerp above (covering all
non-`DestructTree` destruct types), and the `ClipLand` conform (already GPU-gated). `DestructTree`
is a matrix topple; `DestructMan` is skeletal (palette); RTM/movement live in derived overrides.

## 4. Non-goals

- Not touching GL33 behavior. All changes are wgpu-internal or feature-gated (§6).
- Not re-doing conform (done/scoped in the conform plan) or skinning (skin-bake plan).
- Not building procedural/streamed road geometry (compute-generated ribbons) — deferred.
- Not changing gameplay/network determinism of destruction. The destroyed variant is still
  generated by the same seeded `MakeDestroyed`; we only change *where the morph is evaluated*,
  not *what* it produces. Seed reproducibility must be preserved (open question §7.1).

## 5. Cross-cutting design: the per-instance record

Extend `ObjectGpu` (and its shadow-side twin `ShadowCasterGpu`) with the fields the stages below
need. Everything that varies per instance goes here, off the vertex buffer:

| field | added by | consumer |
|---|---|---|
| `world: mat4` | exists | all |
| `conform0/1/2: vec4` | exists | conform (color + shadow) |
| `morph_ratio: f32` | Stage 1 | destruction/keyframe morph in VS |
| `selector: u32` (LOD / destruct-stage / section / vis bits) | Stage 1/3 | VS + Stage 3 cull |
| `palette_offset: u32` | skin-bake plan | skinned VS |
| `texture_index: u32` | Stage 2 | bindless per-instance material |

Define the packing **once** and make color, shadow, and (Stage 3) the cull compute pass all read
the same layout. `ObjectGpu` is already load-bearing for live instanced draws, so extend it
additively (append fields, keep 16-B alignment) rather than reshaping it.

Destruction/keyframe morph also needs a **second position+normal stream** (the destroyed / target
pose) resident alongside the base mesh — see Stage 1.

## 6. GL33 non-regression strategy

The pattern already exists and works — mirror it exactly:

- **Conform** is gated by `GGpuTerrainConform` / `GCurrentConformPlane.active`; GL33 never
  publishes a plane, so it keeps deforming on the CPU. Do the same for the destruction morph:
  a `GGpuShapeMorph`-style flag (set by `EngineWgpu`), and `Object::Animate` skips the CPU
  vertex lerp + `InvalidateBuffer` when it is active *and* a morph ratio was published — exactly
  parallel to [Object.cpp:387](../../Poseidon/World/Scene/Object.cpp#L387). GL33 (flag off) is
  byte-for-byte unchanged.
- **Decal instancing (Stage 2)** is entirely inside the Rust `plan_3d`; the engine emits the same
  `WgrDraw3D` commands. GL33 does not go through this code at all.
- **Gameplay geometry must stay on the CPU.** Occlusion / LOS / fire geometry run the *same*
  `Object::Animate` conform on geometry LODs at intersection time
  (`ObjectIntersect.cpp` → `AnimateComponentLevel`). The morph gate must apply **only to visual
  LODs on the draw path**, never to geometry-LOD queries — otherwise gameplay occluders drift
  from visuals. This is the single highest-risk correctness point; verify it explicitly.

## 7. Staged work plan

Each stage lands as its own reviewable change. Stages 1–2 need no new GPU features.

### Stage 1 — Destruction (and keyframe) morph in the vertex shader

The prize, and a prerequisite for instancing destructible objects. Move the two-endpoint morph
from `Object::Animate` into the VS.

**Geometry residency.** Keep both pose streams resident and static:
- Intact stream = the existing static mesh.
- Destroyed stream = generated once by the existing `MakeDestroyed`/`MakeTreeDestroyed`
  ([ObjTrash.cpp](../../Poseidon/World/Scene/ObjTrash.cpp)); upload once as a second
  position+normal vertex stream. **Generated eagerly at load** (decided — predictable VRAM, no
  mid-frame upload stall on a destruction event).
- **Same topology, order, and index buffer** (the generator only *displaces* the intact vertices — same
  count, same order, same faces). So the destroyed stream is a **pure parallel position+normal buffer**
  at the *same* `base_vertex`, sharing the intact mesh's index ranges and sections — not a separate mesh.
  That is what makes the morph a straight per-vertex `mix` (below), and it means **destruction adds no
  draws, no section descriptors, and never changes draw batching** — only the per-vertex positions the VS
  reads. (Big simplification for the retained scene / cull batching, see
  [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) §2.)
- Morph targets are per-LOD; bind the selected LOD's intact+destroyed streams together.

**Per-instance.** `morph_ratio` in `ObjectGpu` (§5). Publish it around the visual draw (mirror
`PublishConformPlane`), and under the morph flag `Object::Animate` skips the CPU lerp for visual
LODs (§6).

**Vertex shader order** (must match the CPU order):
`pos = mix(intactPos, destroyedPos, ratio)` → **then** conform (destroyed verts carry
`ClipLandKeep`, matching the CPU) → then `world` transform. Blend + renormalize normals
`normalize(mix(nIntact, nDestroyed, ratio))`, accepting a minor delta vs the CPU's
`RecalculateNormals` (verify visually).

**Bounds.** Culling/measure bounds for a collapsing/destroyed instance = union of intact and
destroyed bounds (both static, both known) — conservative and free. Fits the existing
`AnimatedMinMax` cache.

**Fold in RTM keyframes if cheap.** `Shape::SetPhase`/`PointPhase` is the same
blend-between-static-poses shape; if the two nearest phases can be bound as the two streams,
it reuses this path. If it complicates Stage 1, defer to a follow-up (§8).

- Risk: medium (shader correctness, normal quality, the gameplay-LOD gate in §6).
- GL33: unaffected (flag off → existing CPU lerp).

### Stage 2 — Make the decal / on-surface path instanceable

Roads all share one pipeline shape (Decal offset, opaque/cutout, non-skinned) but are excluded
from instancing purely because `Offset::Decal` fails the predicate at
[mod.rs:2120-2123](../rust/src/gfx3d/mod.rs#L2120-L2123). This is a **Rust-only** change.

- Extend the instanceable predicate to admit `Offset::Decal` (and `ZBias`) when the rest matches;
  fold `offset` into `BucketKey` so decal instances batch among themselves but never merge with
  `None`-offset geometry (they need the polygon-offset pipeline variant).
- Keep per-instance material/texture variation in the **bindless `texture_index`** (§5), not in
  `PipelineKey`, so texture differences don't fragment the batch.
- Feed the **same** per-instance buffer to color and shadow so conform stays consistent (the
  shadow path already coalesces — [mod.rs:1517-1642](../rust/src/gfx3d/mod.rs#L1517-L1642)).
- Depends on the conform plan's road work landing first: roads must be *drawable geometry* (B1
  cached split, or B2 baked static VB) before they can be batched. B2 is the natural feed — a
  static road VB drops straight into the instancer. Coordinate ordering with that plan.
- Risk: low. Pipeline state unchanged; only batch grouping changes. Verify decals still win the
  depth test (polygon offset / `WGR_DECAL_SCALE`) after coalescing.

### Stage 3 — GPU-driven submission (specified concretely in the culling plan)

The first stage that stops the CPU iterating objects per frame. **This is now designed in full in
[gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)** — the retained GPU scene model, the
merged geometry pool + bindless object textures, the cull + LOD compute (distance + frustum, LOD select,
compaction), indirect draw (portable `multi_draw_indirect` / single-`draw_indexed_indirect` Metal loop),
and — layered on — Hi-Z occlusion. That plan is the authoritative spec; this stage just names what the
**object plan contributes** to it, so the two don't duplicate.

**This plan owns / feeds into the culling plan:**
- The **per-instance record** (§5 here) — realized as the culling plan's unified retained instance buffer
  (§2.3 there). Same fields, one definition.
- **Per-instance bounds** for the cull test: object bounds from cached `AnimatedMinMax`, **union-with-
  destroyed** for morphing instances; road-segment bounds built at load from `RoadNet` centerlines in
  [Roads.hpp](../../Poseidon/World/Terrain/Roads.hpp) (the AI graph — topology, *not* render geometry).
- The **destruction/keyframe morph** (Stage 1 here) — the culling plan's instances carry `destroy_phase`
  + a destroyed-variant ref (eager, resident at load) and the VS morphs; bounds are the union so a
  mid-morph object never culls out.

Occlusion, LOD math, indirect plumbing, Metal portability, and multi-view (shadows/reflections) are all
detailed there — do not re-plan them here. Start only once Stages 1–2 have fixed the per-instance record
the cull pass reads. Risk: high; largest change.

## 8. Open questions to resolve during investigation

1. **Determinism / lifetime of the destroyed variant.** Confirm generating it *earlier* (eager
   upload at load) vs *lazily on first destruction* does not change the seed or the produced mesh
   — `MakeDestroyed` uses `GRandGen` with a per-model seed
   ([ObjTrash.cpp:78,111](../../Poseidon/World/Scene/ObjTrash.cpp#L78)); verify network
   determinism is unaffected by *when* it runs. Also note the latent precedence bug
   `& 0x3ff + seed` at [ObjTrash.cpp](../../Poseidon/World/Scene/ObjTrash.cpp) — decide whether
   to preserve it bug-for-bug (to match existing content) or fix it (may shift destroyed meshes).
2. **Memory budget** for a second position+normal stream on every destructible model: eager
   (predictable, higher VRAM) vs lazy-on-first-destruction (one-time upload hitch). Count
   destructible models × avg vertices × 24 B.
3. **Gameplay-LOD gate (§6).** Prove the morph skip touches only visual LODs, never the geometry
   LODs used by `ObjectIntersect`. This is the correctness gate; do it before writing the skip.
4. **Normal quality** during morph: is `mix`+renormalize acceptable vs `RecalculateNormals`? Any
   content relying on exact destroyed-mesh normals?
5. **Shader conform order** on destroyed verts: confirm `ClipLandKeep` handling matches the CPU
   (morph → conform → transform).
6. **RTM fold-in (Stage 1)** now or as a follow-up — does `PointPhase`'s two-nearest-phase blend
   map cleanly onto the two-stream morph?
7. **Decal depth after coalescing (Stage 2):** confirm polygon offset / `ZRoadEpsilon` /
   `WGR_DECAL_SCALE` still resolve roads above terrain once batched.
8. **Stage 3 features/fallbacks:** availability of `MULTI_DRAW_INDIRECT[_COUNT]` on the target
   adapters; where the cull pass reads per-instance bounds; union-of-endpoints bounds for
   morphing instances.

## 9. Suggested starting point

1. Read `gfx3d/mod.rs` `plan_3d` + the instanceable predicate end to end; confirm §3 against the
   current code and note any drift.
2. Scope **Stage 1** first — it's independent of the conform/road plan and delivers the
   architecturally-required morph. Produce the `ObjectGpu` extension (§5), the second-stream
   upload path, the VS morph, and the §6 gate, as a PR-sized task list. Resolve open questions
   1, 3, 4 before writing the skip.
3. Coordinate **Stage 2** ordering with the conform plan's road Part B (B2 baked VB is the feed).
4. Do **not** begin Stage 3 until 1–2 fix the per-instance record the cull pass consumes.
