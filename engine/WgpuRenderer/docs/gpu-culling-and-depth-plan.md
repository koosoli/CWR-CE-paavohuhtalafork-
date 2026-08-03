# Plan: GPU-driven rendering — retained scene, cull + LOD, indirect draw, Hi-Z occlusion

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
> **Stage audit (2026-08-03).** The status block below still reports Stage 3 as
> "Rust DONE (`WGR_GPU_DRIVEN`, inert); ⛔ C++ feed + count-trim remain". **Both have landed.**
> `wgr_instance_add` is called from a `_gpuDriven`-gated hook in `EngineWgpu`, and count-trim
> exists as `multi_draw_count_enabled`. Stage 3 is live in a shipped client, because
> `ConfigureWgpuUltraEnvironment` sets `WGR_GPU_DRIVEN=1` before the engine is created.
>
> Stages 5 (multi-view) and 6 (skinned + transparent) remain unstarted. Stage 6 is where
> `compute-skin-bake` is meant to be re-enabled.
>
> This stale line caused a wrong conclusion in the RND-030 consolidation record before it was
> caught — see [`RND-030-renderer-consolidation-20260803.md`](../../../docs/roadmap/decisions/RND-030-renderer-consolidation-20260803.md) §4.

**Status:** IN PROGRESS (2026-07-09). Stages 1–3 are LIVE and user-verified in-game under
`WGR_GPU_DRIVEN=1`: static opaque rigid clutter + ForestPlain forests (mode-1 conform) + individual
ClipLand vegetation/fences (mode-2 conform) are culled, LOD-selected and drawn entirely on the GPU,
participate in the depth+normal prepass, cast their own **GPU-driven cascade shadows** (§6 multi-view,
2026-07-11), and the retained scene is kept correct event-driven (hooks at every transform mover, incl.
the terrain-subdivision re-seat). **Hi-Z occlusion (§5) LANDED (2026-07-11)**: color-pass GPU Hi-Z
occlusion, terrain-as-occluder, built-in CPU occlusion disabled while active, ImGui-toggleable. **Instancing
collapse (§3.6) LANDED + user-verified in-game + RenderDoc (2026-07-11)**: the cull is now a three-pass
COUNT→EMIT→SCATTER that emits one INSTANCED draw per surviving section instead of one per
(instance,section) pair. **CPU-render divert IMPLEMENTED (2026-07-11, pending in-game verify)**: FULL-coverage
GPU objects no longer get a `SortObject` (one guard in `Scene::ObjectForDrawing`), taking them out of the
whole Pass1 walk — image-invariant (DrawSortObject already skipped them), pure CPU-work removal. Remaining:
dynamics/skinned/transparent/multi-view (reflections). Everything is on branch
`new-renderer-infrastructure`, gated, A/B-toggleable.
**Roadmap slot:** Phase 4 — see [implementation-roadmap.md](implementation-roadmap.md).

### Implementation status (2026-07-09)

The coarse Stage 1–6 below was sub-divided in practice. What's **DONE** (Rust-side, in `engine/WgpuRenderer/rust/src/gfx3d/`):

- **Geometry pool** (`pool.rs`) — merged Uint32 vbuf/ibuf + free-list; every mesh suballocates. `WGR_*` none (always on). Verified in-game. *(Was "Stage 1" here; the rest of the doc's Stage 1 — model/LOD table, instance buffer — landed in the cull work below instead.)*
- **CPU-built indirect** (`draw_indirect` in `mod.rs`, `DrawKind` in `plan_3d`) — `WGR_INDIRECT` (default on when `INDIRECT_FIRST_INSTANCE` present). Verified in-game. This is the doc's **Stage 2**.
- **GPU cull+LOD compute** (`cull.rs` + `cull.wgsl`) — frustum + near-clamp distance + sub-pixel cull + `FindSqrtLevel`-shaped LOD (tunable, **no exact-parity requirement** per the user — push LODs further once the CPU is out of the loop), atomic-append `DrawIndexedIndirect` per surviving (section,instance) into per-variant partitions + a `{instance,section}` record. Headless GPU test. Doc's **Stage 3, §3**.
- **Retained-scene buffers** (`CullState` in `cull.rs`) — unified instance buffer (static slots + free-list, `model=INVALID_MODEL` holes; dynamic region re-copied each frame), model/LOD/section/section-material tables, cull-params uniform, out_args/out_records/counters. `register_model` / instance add-update-remove / set_dynamic / prepare / dispatch. Doc's **Stage 3, §2**.
- **GPU-driven draw** (`gpu_driven.wgsl` + `mod.rs`) — `multi_draw_indexed_indirect` per variant; VS reads the record→instance world (absolute→cam-relative in-shader), FS folds raw section material × frame sun and calls the shared `shade()` (`shaders/shading.wgsl`, extracted from `fs_main` — verified the per-draw path is visually unchanged). `WGR_GPU_DRIVEN` (default **off**). Doc's **Stage 3, §3.4/§4**.

**Sub-stage log** (originally "not yet done"; ✅ items landed and were user-verified in-game 2026-07-09):

- **C++ retained scene ("3b-3") — ✅ IMPLEMENTED + VALIDATED IN-GAME (2026-07-09, uncommitted).** FFI (`wgr_model_register` / `wgr_instance_add`/`update`/`remove` / `wgr_set_dynamic`) mirrored C++↔Rust (header + ffi.rs + Gfx3d/Renderer wrappers). Engine feed via **new no-op `Engine` virtuals** `SceneObjectCreated`/`Removed`/`Moved` + `GpuDrivenObject` (only `EngineWgpu` implements them; gated on `WGR_GPU_DRIVEN==1` read in the ctor). `EngineWgpu::RegisterGpuModel(LODShapeWithShadow*)` lazily builds the model — **owns its geometry** (`wgr_mesh_create` from `BuildVertices`/`BuildIndices`/`BuildSections`, stored in `_gpuMeshes`; borrowing the shape's `_buffer` failed — `ShapeBank::OptimizeAll`→`ReleaseAllVBuffers` frees them during world load) — graphical `IsNormalLevel` LODs → per-section raw material via `CreateMaterial(HWhite,section.material)`+`surfMat->Combine`, texture via `EnsureUploaded`→bindless slot, `SamplerForSpec`/`AlphaRefFromDesc`; **eligibility = opaque `BlendMode::Opaque` + `SurfaceMode::Default`, `NProxies()==0`** (proxy-bearing buildings stay CPU: interior furniture are proxy marker triangles drawn inline by `DrawProxies`; the nulled markers would bake as white triangles and the furniture would vanish), hidden sections (`IsHidden|IsHiddenProxy`) skipped; any transparent/decal section ⇒ whole shape stays CPU, cached as `WGR_INVALID_MODEL`. Terrain-conform shapes ARE eligible (see the conform bullet below). Instances streamed at `Landscape::AddObject`→add / `RemoveObject`+`ReleaseObjects`(cell unload — direct-delete, needed its own hook)→remove / `MoveObject`→update; **destruction** (`Object::SetDestroyed` first `_isDestroyed` edge)→remove (drops to CPU, which draws the destroyed geometry — the GPU path holds intact geometry only). CPU colour draw suppressed in `DrawSortObject` via `GpuDrivenObject` (shadows stay CPU — `SceneShadowPass` is separate). **Scope: static (`Static()`) opaque-rigid clutter only**; dynamics/skinned/transparent stay CPU.
- ~~**`MULTI_DRAW_INDIRECT_COUNT` count-buffer trim ("3b-4")**~~ **✅ DONE (2026-07-09).** The cull counter buffer is now a dedicated `STORAGE|INDIRECT|COPY_DST` buffer (fixed `CULL_VARIANT_COUNT` words, `cull.rs`), doubling as the count buffer. `lib.rs` gates `Features::MULTI_DRAW_INDIRECT_COUNT` (adapter-optional, requested when present). `Gfx3d.multi_draw_count_enabled` (= `gpu_driven_enabled && feature present`) switches `draw_gpu_driven` between `multi_draw_indexed_indirect_count(args, v·cap·20, counters, v·4, cap)` (desktop trim) and the conservative `multi_draw_indexed_indirect(args, off, cap)` no-op-tail fallback (Metal). Compiles + clippy clean + 13 tests green.
- ~~**Cull correctness (frustum / LOD scale / bounding sphere)**~~ **✅ FIXED + user-confirmed working (2026-07-09).** Three bugs found once the path ran live: **(a) LOD/distance scale** — the compute hardcoded `lod_scale=lod_inv_width=1`, but the legacy `resol2 = dist²·_lodInvWidth²·Camera::Left()²` has `_lodInvWidth ≈ lodCoef·2/screenWidth ≈ 1e-3` and `Left() ≈ 0.75`, so `resol2` was ~1e6× too large (every model → coarsest LOD within metres). Fixed by plumbing the real values through new FFI `wgr_set_cull_params(objects_z, lod_scale, lod_inv_width, pixel_limit)`, pushed each frame from `PushSceneCamera`; sub-pixel cull re-enabled at legacy 0.125. **(b) Near-plane normal** — the engine is D3D-style **left-handed** (row-major `GfxMatrix`, `w_clip=+z_view`); a hand-derived forward (`-Z`, then `view.z_axis`) pointed wrong and culled a direction-dependent half-space (objects popped in/out on rotation). Fixed: **all six planes now come from `proj*view` directly** — `frustum_planes` takes only `view_proj`, near = `row3` (the `clip.w≥0` half-space, projection-consistent for any orientation). **(c) Cull-sphere center** — `BuildGpuInstance` used `Transform·BoundingCenter()`, but `CalculateBoundingSphere` re-centers the stored vertices around the vertex-space origin (`pos -= changeBoundingCenter`) and `_boundingSphere` is the radius about THAT origin, drawn at `Transform.Position()`. Using `BoundingCenter` offset the sphere and culled offset-origin objects early / when touching. Fixed: **center = `Transform.Position()`** (matches legacy `SceneDraw.cpp` center=`trans.Position()`, radius=`BoundingSphere()·Scale`). Rotated-camera frustum↔projection consistency test added (`frustum_matches_projection_rotated`).
- **GPU-driven set draws in the COLOUR PASS ONLY — two known gaps + the further-out work:**
  - ~~**(gap 1) Depth+normal PREPASS participation**~~ **✅ DONE + confirmed in-game (2026-07-09, Rust-only, uncommitted).** `build_gpu_pipeline` now returns BOTH the colour pipeline (`fs_gpu`) and a prepass pipeline (`fs_gpu_prepass`) from one shared module; `gpu_driven.wgsl` gained `fs_gpu_prepass` (imports `gbuffer::oct_encode`, discards cutout via the per-section alpha_ref, writes the view-space octahedral normal into `NORMAL_FORMAT` — mirrors shader3d's `fs_prepass`). `Gfx3d.gpu_prepass_pipeline` + `draw_gpu_driven_prepass` (shares `draw_gpu_driven_impl`, same per-frame cull `out_args` — the cull dispatch already ran before both passes). `lib.rs` calls it inside the `do_prepass` block (world segment) before the pass is dropped, so the GPU-driven set now writes prepass depth (⇒ early-Z in the colour pass) and populates the view-space normal G-buffer at parity with the CPU set. **(SSAO itself is not implemented yet — a future plan; this just makes the GPU-driven set a first-class prepass participant so SSAO/any prepass-depth consumer covers it for free when it lands.)** Colour-pass GPU draw kept depth-write ON (GreaterEqual re-passes on equal depth — harmless; a depth-write-off colour variant is a later micro-opt). Composer test validates `fs_gpu_prepass`; clippy + 15 tests green.
  - ~~**(gap 2) Instancing collapse**~~ **✅ DONE + user-verified in-game + RenderDoc (2026-07-11, Rust-only, uncommitted; no glitches, draws compacted as expected).** The cull is a three-pass COUNT→EMIT→SCATTER (see §3.6): COUNT (1/instance) tallies survivors per global section into a `sec_count` scratch; EMIT (1/section) bump-allocates the section's contiguous `out_records` run (global cursor folded into the counter buffer as its trailing word) and emits ONE instanced `DrawArgs{instance_count = c, first_instance = run_base}`; SCATTER (1/instance) fills the runs. Arg count drops from N·S pairs to (# surviving sections). classify()/occluded() are pure so COUNT and SCATTER agree on survivors — no `inst_lod` buffer (recomputed, saving a binding: the whole layout fits the default 8-storage-buffer limit with one added scratch binding). Three separate `begin_compute_pass` for the wgpu inter-pass storage barrier. VS/FS/C++ unchanged (`first_instance + i` still indexes `out_records`). Applies to the main, every shadow cascade, AND the color-occlusion view. Test `instancing_collapse_end_to_end` pins K-collapse + contiguous runs + per-LOD split.
- ~~**Terrain-conforming objects (§11) — BOTH modes**~~ **✅ DONE + user-verified in-game (2026-07-09).** Final architecture: `WgrInstance`/`InstanceGpu` grew 96→144 B with `conform0/1/2` vec4s mirroring the per-draw `WgrDraw3D` conform exactly; `conform2.z` = mode (0 rigid / 1 ForestPlain bilinear land-grid plane / 2 per-vertex ClipLand SurfaceY with `conform0.x = bcSurfaceY`). `gpu_driven.wgsl` `vs_gpu` conform block is a near-verbatim copy of `shader3d.wgsl` `vs_main` (group(4) heightmap `surface_y`/`surface_grad`, `@location(5) conform_sel`, normal tilt); VS-only so the prepass inherits it. Mode 1 reads `ForestPlain::GpuConformPlane()` (cached `_conformPlane`; skewed t1/t2 squares bake conform into the transform → register rigid). Mode-2 baking requires **`s->SaveOriginalPos()` in `RegisterGpuModel`** (`BuildOrigVertices` bakes `conform_sel` from `OrigClip`; `Object::Animate` only saves origs at draw time, and plain `BuildVertices` hardcodes `conform_sel=0` → rigid float). `ConformPlane{}` defaults `mode=1` — rigid must set `mode=0` explicitly.
- **Retained-scene correctness = event-driven hooks at EVERY transform mover — ✅ DONE + user-verified (2026-07-09).** The load-bearing find: **`Landscape::MakeObjectsTerrainRelative/Absolute`** re-seats every non-ForestPlain static by `SurfaceY_new − SurfaceY_old` via direct `SetPosition` (no `MoveObject`) around every heightfield change (`SubdivideTerrain`/`ResampleTerrain`/`LoadSubdivCache`, all via `World::AdjustSubdivision` — which fires at world load AFTER `LoadObjects`, at mission start, `setTerrainGrid`, options, MP join). Stale registrations presented as BOTH "mode-2 bushes vanish at horizon pitch" (cull sphere 4–17 m above the conformed draw) and "rigid trees float". Fix: `MakeObjectsTerrainAbsolute` fires `SceneObjectMoved` per re-seated object (final positions only; `Relative`'s intermediate Y is never synced). A drift **tripwire** in `GpuDrivenObject` (live vs stored position) LOG_WARNs + refreshes if any future mover is unhooked — should never fire; it rides the CPU visible-set walk and is deleted with the divert. Cull sphere = raw `Transform.Position()` + `BoundingSphere()·Scale()`, exactly the CPU's — no conforming, no inflation (both tried, both wrong). The wgpu heightmap tracks subdivision (range change trips `UploadIfNeeded`); a same-range in-place height edit would go stale silently (no content check) — future terrain-deform work must version it.
- **Debug tooling — ImGui dev-panel "Culling" tab (permanent, `WGR_GPU_DRIVEN=1`):** *Draw cull spheres* (instanced line-list wireframe per retained instance from the REAL instance/model buffers — `cull_debug.wgsl`; colour by mode; depth Always, drawn last in the sub-pass), *Disable frustum cull* (CullParams flag; also `WGR_CULL_NO_FRUSTUM`), *Dump nearby instances* (≤48 within 60 m: live vs registration-time position vs surface — the `stale` column is what cracked the settle bug). Per-bool FFI setters (`wgr_set_cull_debug`) + `CullDebugSettings` Engine virtuals mirror the Water-tab pattern. Also: `DEFAULT_VARIANT_CAPACITY` raised to 256K/variant — arg overflow drops follow per-frame atomic order, so overflow = random every-other-frame flicker (that signature means capacity).
- **GPU-path shadow casting (§6 multi-view, first slice) — ✅ IMPLEMENTED + verified (2026-07-11, RenderDoc + in-game, uncommitted).** The retained set now casts its OWN cascade shadows on the GPU; the matching CPU casters are suppressed, so a GPU-driven object is out of the CPU shadow loop too. **Rust:** `CullState` grew per-cascade **`ShadowCullView`s** (own params/out_args/out_records/counters/bind, all referencing the SHARED instance+table buffers — `cull.wgsl` is view-agnostic, so **no compute change**; multi-view is "the same cull with a different frustum"). Each cascade's params come from `params_from_shadow_cascade(light_vp[c], shadow.cam_pos, main_cull_inputs)` — frustum extracted from the camera-relative light-VP via the existing `frustum_planes` (ortho ⇒ 4 working side planes + degenerate near/far no-ops, correct because casters outside the cascade depth range are NDC-z-clipped in the depth pass), LOD from the **main** view (a caster's shadow uses the same LOD its colour draw does), and the radial **distance cull disabled** (`objects_z2 = 1e30`) so the far cascades aren't clipped by the main draw distance. New depth-only pipeline (`build_gpu_shadow_pipeline`) + shader **`gpu_driven_shadow.wgsl`**: VS reads records→instance→world, conforms per vertex (mode 1/2 verbatim from `vs_gpu`), `clip = light_vp·(world_abs − cam_pos)`; FS discards cutout foliage below the per-section `alpha_ref` (one pipeline serves both variants; solids never sample). Forward-Z / SHADOW_FORMAT / CW / no-cull / same depth bias as the CPU caster pipeline. Frame: one extra cull dispatch per cascade (`cull_dispatch_shadows`) after the main cull; the GPU set is drawn into each cascade's existing depth pass inside `render_shadow_passes` (which no longer early-returns on empty CPU casters — `prepare_shadows` sets up the target + pass-UBO whenever `gpu_driven` is on). **C++ suppression** (mirrors the §12 colour path, gated by `WGR_GPU_DRIVEN`): in `Scene::RenderShadowMapDepthPassGpu`, a **`Full`** object (+ its GPU proxy children, which are resident instances the cascade cull casts on its own) skips its CPU caster entirely; a **`Partial`** object casts only its complement via `AddShadowCaster` under `GSkipGpuOwnedSections` (which now drops `render::IsGpuOwnedSectionSpec` sections — the SAME predicate `ClassifyGpuSection`/`Shape::Draw` use, so CPU + GPU never overlap or leave a hole); GPU-handed proxies are skipped in the proxy loop via `GpuDrivenProxy`. Headless multi-view test (`shadow_cull_view_end_to_end`) + shader-compose test added; clippy + 16 lib tests green. **Ahead:** Hi-Z occlusion is per-view so shadow views stay frustum+distance+LOD only (correct — §6); the shadow cull inherits §3.6 (per-(instance,section) sub-draws, no instancing collapse yet) and the shadow LOD's coarser `casterLodBias`/`shadowFar` distance-bound nuances are not yet ported (parity gap: distant tiny casters lean on the shared sub-pixel cull instead).
- **Hi-Z occlusion (§5) — ✅ IMPLEMENTED (2026-07-11).** See §5 for the full architecture. Prepass-based,
  color-pass only, terrain-as-occluder free, `EnableObjOcc` (built-in CPU occlusion) forced off while
  active, ImGui-toggleable (`WGR_GPU_OCCLUSION` / Culling tab). Real-GPU headless test pins the reversed-Z
  min direction.
- **Destroyed variants (§2.4), multi-view reflections (§6), dynamics feed, skinned/transparent (§6)** — all still ahead (destruction currently drops the object to the CPU path).

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
  view-distance-governed bias. Lean (b). *(Currently (c) — fixed bias fed from `Camera::Left()·_lodInvWidth` via `wgr_set_cull_params`.)*

### 3.6 Instancing collapse (gap 2) — concrete plan

> **Status: ✅ DONE + user-verified in-game + RenderDoc (2026-07-11, Rust-only, uncommitted; no
> graphical glitches, draw commands compacted as expected).** Implemented as the
> three-pass COUNT→EMIT→SCATTER below, with two deviations that shrank the binding budget to fit the
> default 8-storage-buffer limit: (a) **no `inst_lod` scratch** — SCATTER re-runs the pure `classify()`
> (and `occluded()` for the color view) instead of caching the LOD, so pass A/C see identical survivors;
> (b) the **records cursor is folded into the counter buffer** as its trailing word (index
> `variant_count`) rather than a separate binding — the count-buffer trim only reads words
> `0..variant_count`, so it's invisible. Only ONE new binding (9 = the `sec_count` per-section scratch,
> reused across all three passes: COUNT adds into it, EMIT overwrites it with the run base, SCATTER bumps
> it). `out_records` is now a flat global array (not per-variant partitioned); overflow is guarded at EMIT
> (reserve-before-cap so un-emitted sections still get a unique base → no SCATTER collision at base 0) and
> at SCATTER (bounds-checked writes). Applies to the main, every shadow cascade, and the color-occlusion
> view. Test: `instancing_collapse_end_to_end`.

**Today (shipped):** `cull.wgsl` emits one `DrawIndexedIndirect` per surviving **(instance, section)** with
`instance_count = 1`, `first_instance` = the record slot, `out_records[slot] = {instance, section}` 1:1 with
args. So *N* instances × *S* sections = *N·S* sub-draws, *N·S* args, *N·S* records. The CPU draw-call cost is
already amortized (one `multi_draw` per variant), so the cost is GPU sub-draw setup + the `out_args` size, not
API overhead — a throughput optimization, not correctness.

**Target:** one `DrawIndexedIndirect` per surviving **global section** with `instance_count =` (# instances that
selected a LOD containing it), its instances laid out **contiguously** in `out_records` at
`first_instance = run_base`. Args drop from *N·S* to *(# distinct surviving sections)* (≤ registered-section
count); records stay *N·S*. The section id already encodes (model, LOD, section), so grouping key = **global
section id** — instances at different distances pick different LODs → different sections → separate draws,
automatically.

**Do we need a parallel prefix sum? No.** The contiguous per-section record runs are carved with a single
**atomic bump per section** (one thread per global section, `atomicAdd` on a global records cursor), not a
scan. At our scale (#sections ~10³–10⁴, #instances ~10⁴–10⁵) a full scan buys nothing; the per-variant arg
compaction already uses atomic-append (§3.3) and works. There is **no batteries-included wgpu scan worth a
dependency**; if we ever needed one (to kill atomic contention at 10⁶+ items) we'd write a standard workgroup
scan — subgroup ops (`subgroupExclusiveAdd`) are available via `Features::SUBGROUPS`, which we do **not**
currently request. **Recommendation: atomics, no scan.**

**Three-pass compute** (replaces the single cull dispatch; three separate `begin_compute_pass` calls — wgpu
auto-inserts the storage barriers between them, as the terrain/sky computes rely on):

- **Scratch buffers** (per frame; `clear_buffer` the counters before pass A):
  - `sec_count[s]: atomic<u32>` — surviving instances drawing global section `s` (sized `sections.len()`).
  - `sec_fill[s]: atomic<u32>` — fill cursor within `s`'s run (pass C); seeded to the run base in pass B.
  - `inst_lod[i]: u32` — chosen LOD level, or `CULLED` sentinel (sized instances). Written pass A, read pass
    C — avoids recomputing the cull/LOD test.
  - `records_cursor: atomic<u32>` — bump allocator over `out_records`.
  - `arg_counter[variant]: atomic<u32>` — per-variant arg append cursor (unchanged; doubles as the count
    buffer for the `MULTI_DRAW_INDIRECT_COUNT` trim).
- **Pass A — count** (1 thread / instance): distance+frustum+sub-pixel cull → if culled `inst_lod[i]=CULLED;
  return`. LOD select → `lod`; `inst_lod[i]=lod`. For each section `s` of the chosen LOD: `atomicAdd(sec_count[s], 1)`.
- **Pass B — allocate + emit args** (1 thread / global section `s`): `c = sec_count[s]; if c==0 return`.
  `base = atomicAdd(records_cursor, c); sec_fill[s] = base`. `v = sections[s].variant;
  slot = atomicAdd(arg_counter[v], 1); if slot >= cap { return }` (overflow → detected via the counter
  readback, as today). `out_args[v*cap + slot] = DrawArgs{ index_count, instance_count = c, first_index,
  base_vertex, first_instance = base }`. (All geometry fields come from `sections[s]` — no instance needed.)
- **Pass C — scatter** (1 thread / instance): `lod = inst_lod[i]; if CULLED return`. For each section `s` of the
  LOD: `slot = atomicAdd(sec_fill[s], 1); out_records[slot] = Record{ instance: i, section: s }`.

**VS / FS / draw: UNCHANGED.** A sub-draw's `@builtin(instance_index) = first_instance + i = base + i`
(`INDIRECT_FIRST_INSTANCE`), which indexes `out_records[base+i] = {instance, section}` exactly as today.
`vs_gpu`/`fs_gpu`/`fs_gpu_prepass` already read `records[instance_index]` — nothing to change. Only the
compaction differs. The `multi_draw_indexed_indirect_count` fast path is unchanged and now trims a *much*
smaller arg list; the Metal conservative-cap fallback also shrinks (cap covers # sections, not # pairs).

**Sizing / clears:** `out_args` capacity can drop to `sections.len()` (exact upper bound) from the current
`CULL_VARIANT_COUNT · 64K`; `out_records` unchanged. Instance order within a run is arbitrary (atomic race) —
fine; instancing is order-independent, no sort.

**Verification:** extend the headless end-to-end test — K instances at one distance (same LOD) ⇒ **one** arg
per section with `instance_count = K` + K contiguous records; instances spread across distances ⇒ separate
args per LOD. Effort: ~medium (2 extra dispatches + scratch buffers + split `main` into count/scatter entries +
a per-section pass B; VS/FS/C++ untouched).

---

## 4. Indirect draw plumbing — portable, no Metal-specific code

**Design principle: the GPU cull + compaction is identical on every backend, and so is the submission
call.** Verified against wgpu 29.0.1 source (correcting the earlier assumption that multi-draw is
Metal-gated):

- **`multi_draw_indexed_indirect(args, offset, count)` is a plain `RenderPass` method requiring only
  `DownlevelFlags::INDIRECT_EXECUTION`** — a downlevel capability present on every native desktop backend
  (Vulkan/DX12/Metal/GL), **not** a `Features` gate. On backends without native multi-draw (Metal, GL) wgpu
  **emulates it internally as a loop of single draws**. So there is **no `MULTI_DRAW_INDIRECT` feature and
  no hand-written Metal loop** — one `multi_draw_indexed_indirect` per pipeline variant is portable as-is.
- **`multi_draw_indexed_indirect_count`** (the GPU **count buffer** that skips empty draws) is the separate
  feature, gated on **`Features::MULTI_DRAW_INDIRECT_COUNT`**, which **Metal lacks**. This is the only
  count-related portability split. Without it, pass `count` = a CPU/conservative upper bound and leave the
  compaction's unused slots as `instance_count == 0` **no-op draws** (still one `multi_draw` call, wgpu
  emulates the loop on Metal). With it (Vulkan/DX12), the GPU count trims the emitted-but-empty tail.
- **`first_instance` from the args still needs `Features::INDIRECT_FIRST_INSTANCE`** (verified present as a
  real feature; this is the actual gate our base_instance model depends on). Absent → encode the
  instance-list base another way (per-batch offset uniform / a base baked into the instance list), or fall
  back to the direct path. Add `BufferUsages::INDIRECT` to the args buffer. Adapter-gated exactly like
  `partially_bound` ([lib.rs](../rust/src/lib.rs)).
- **Last-resort fallback:** if an adapter lacks the compute/storage prerequisites, fall back to the CPU
  `plan_3d` path entirely — it stays as the correctness/A-B reference and the GL33-parity route regardless.
- **Derisk order:** first build a **CPU-produced indirect** path (replay today's `plan_3d` buckets — proves
  the indirect plumbing, geometry pool, bindless textures on real hardware without the compute), *then*
  swap in the compute-produced args (§3). **DONE (Stage 2, 2026-07-09):** implemented with single
  `draw_indexed_indirect` per bucket (simplest proof of the args path); multi-draw grouping deferred to
  Stage 3, where the GPU emits one contiguous per-variant arg array → one `multi_draw_indexed_indirect` per
  variant, portably, no backend branch.

## 5. Hi-Z occlusion (prepass-based)

> **Status: ✅ IMPLEMENTED (2026-07-11, uncommitted, pending in-game verify).** Prepass-based Hi-Z
> occlusion for the color pass, default **on** when GPU-driven is on (`WGR_GPU_OCCLUSION`, default 1;
> live toggle in the ImGui **Culling** tab). Frame order (main view): main cull (frustum+dist+LOD, the
> **occluder/prepass set**) → depth prepass (terrain + CPU objects + GPU-driven set all write the shared
> depth, so **terrain is a first-class occluder for free**) → **Hi-Z build** → **color occlusion cull**
> (`main_occlude`) → color pass draws the occlusion-culled subset. The **prepass draws the full in-frustum
> set** (it generates the occluders — never occlusion-culled, or it'd be circular); only the **color pass**
> gets the occluded subset. Shadow cascades stay frustum+dist+LOD (occlusion is per-view). **Built-in CPU
> occlusion** (`EnableObjOcc`, the software 256² occlusion-buffer rasterizer, Scene.cpp) is **forced off**
> per frame while GPU occlusion is active (driven from `PushSceneCamera`) — GPU Hi-Z replaces it for the
> retained set.
>
> **Architecture (Rust):** a new `hiz` module (`hiz.rs`/`hiz.wgsl`) owns an **R32Float mip pyramid** (depth
> is neither storage-writable nor, as Depth24PlusStencil8, copyable, so mip0 is a compute copy of the depth
> aspect and each mip a 2×2 **min** of the previous, with odd-edge fold-in so no occluder is dropped). Depth
> got `TEXTURE_BINDING` + a DepthOnly-aspect view. `CullState` grew a **color-occlusion view** (a second
> `main_occlude` compute pipeline + layout with the Hi-Z at binding 8; its own params/args/records/counters/
> bind), run by `dispatch_color` after the Hi-Z build; its args feed a dedicated color draw bind
> (`gpu_color_group1_bind`). `CullParamsGpu` grew the occlusion tail (camera-relative `view_proj`, viewport,
> mip count, enable). The color draw picks the occlusion args when `occlusion_active()`, else the main args
> (identical pre-occlusion behaviour — clean A/B, and the toggle falls back live). FFI:
> `wgr_set_cull_debug` gained an `occlusion` bool; `CullDebugSettings.occlusion` (default true) drives it.
>
> **The occlusion test** (`occluded()` in cull.wgsl): project the instance's world-space bounding-sphere
> AABB (8 corners) to screen via `view_proj`, take the screen rect + the nearest reversed-Z (max over
> corners), pick the mip whose texel spans the rect (`ceil(log2(max screen extent px))`), sample the 4 rect
> corners at that mip and **min** them (the farthest occluder over the region), and cull when the sphere's
> **nearest** point is still behind (smaller reversed-Z than) that farthest occluder — the conservative
> Hi-Z test. **⚠️ reverse_z remap (bug fixed 2026-07-11):** `frame.proj` is a FORWARD projection and the
> pipelines apply `reverse_z` (`z = w−z`) in-shader, so the depth buffer + Hi-Z are reversed-Z; `occluded()`
> must compute `depth = (w−z)/w`, not raw `z/w` (the first cut compared forward depth against a reversed
> Hi-Z → culled nothing; frustum cull never noticed since it only uses x/y/w). The headless test now uses a
> forward proj + a mid-value Hi-Z case so it can't regress. **Bails (keeps visible)** whenever it can't test safely: a bound crossing the near plane
> (clip.w ≤ 0), or a screen rect leaving the viewport (edge texels carry no coverage for the off-screen
> part → would over-cull side-poking objects). **Reversed-Z ⇒ min** everywhere — the headline hazard,
> pinned by a real-GPU headless test (`color_occlusion_end_to_end`: a constant-far pyramid must not occlude,
> a constant-near pyramid must). Next: shadow-view Hi-Z is deliberately out (per-view, §6); instancing
> collapse (§3.6) still applies to both cull sets.

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
> **Status: CSM cascades ✅ LANDED (2026-07-11)** — the retained set casts GPU-driven cascade shadows
> via per-cascade `ShadowCullView`s (see the Implementation-status block). The pass is parameterized by
> the cascade's light-VP (frustum) with main-view LOD + distance-cull-off; occlusion stays off for shadow
> views (per-view Hi-Z, below). Reflections (Phase 5) reuse the same machinery + a clip plane, not yet built.
- Parameterize the cull+LOD+indirect pass by an **arbitrary camera + optional clip plane**. The same
  machinery then serves: the **depth prepass** (main camera, no occlusion), the **color pass** (main
  camera + occlusion), **CSM cascades** (each cascade's frustum — replaces the CPU `prepare_shadows`
  compaction, [gfx3d/mod.rs:1562](../rust/src/gfx3d/mod.rs#L1562)), and **planar water reflections**
  (mirrored camera + waterline clip plane, [water-rendering-plan.md](water-rendering-plan.md) Stage 4b /
  roadmap Phase 5). Frustum+distance+LOD reuse per view trivially; occlusion is per-view (needs that
  view's own Hi-Z), so shadow/reflection views use frustum+distance+LOD only.

## 7. Staging (buildable increments)

Each stage ships and is measurable; the CPU path stays as the A/B reference until Stage 3. (See the
Implementation status block at the top for how this maps to the sub-stages actually built + what remains.)

- **Stage 1 — Retained data model (no compute yet).** *(Built as: the geometry pool only. The rest —
  model/LOD table, unified instance buffer — was folded into Stage 3's `CullState` instead of a CPU-draw
  intermediary, since the doc's "CPU still builds draws from this model" step had no consumer once the
  cull compute existed. Patch-stream free-list = the instance free-list; eager destroyed variants = NOT
  yet done.)* Geometry pool (merged vbuf/ibuf + section descriptors), bindless object textures, unified
  instance buffer, model/LOD table, patch stream + free-list, eager destroyed variants. The **CPU still
  builds draws** by reading this model (via the existing `plan_3d`/`draw_one`).
- **Stage 2 — CPU-built indirect. ✅ DONE + verified in-game.** Replay the `plan_3d` buckets as (single)
  `draw_indexed_indirect` over the pool. Proves indirect + pool + bindless with no compute. Feature-gated
  (`WGR_INDIRECT`) + CPU fallback.
- **Stage 3 — GPU cull + LOD compute → compute-built indirect (opaque set). ✅ Rust DONE (`WGR_GPU_DRIVEN`,
  inert); ⛔ C++ feed + count-trim remain.** §3 in full: frustum + distance + LOD select + compaction. The
  CPU stops walking opaque objects. Occlusion **off**. This is the headline win — but only observable once
  the C++ retained-scene feed lands (see Implementation status).
- **Stage 4 — Hi-Z + occlusion. ✅ DONE (2026-07-11).** §5: prepass → Hi-Z (`min`) → occlusion test in a
  dedicated color-pass cull view; built-in CPU occlusion (`EnableObjOcc`) forced off while active.
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
- **Instance-list / indirect-args overflow** — size the args + instance-list buffers for a worst case and
  `log()` if a frame would exceed the per-variant `count` cap; never silently drop draws. (This is a
  uniform hazard on every backend now, not a Metal-only one — `multi_draw_indexed_indirect` is core with
  internal Metal emulation, §4.)
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
- [gpu-terrain-water-cull-plan.md](gpu-terrain-water-cull-plan.md) — the terrain/water half of the §6 multi-view cull: moving CDLOD selection (a static quadtree, currently CPU) onto the GPU via a wavefront compute; ROI is multi-view (prepass + N CSM cascades + reflection), marginal for the main view alone.
- [compute-skin-bake-plan.md](compute-skin-bake-plan.md) — bakes skinned meshes into the pool so they instance (Stage 6).
- [rendering-performance-plan.md](rendering-performance-plan.md) — this is Stage 4 of the umbrella perf roadmap made concrete.

## 11. Terrain-conforming objects on the GPU-driven path (priority — expands coverage)

**Motivation:** the GPU-driven set is *static opaque-rigid clutter only*. `RegisterGpuModel` explicitly rejects
any shape with a `ClipLandKeep | ClipLandOn` hint (`EngineWgpu.cpp:1701`), so **all terrain-conforming
vegetation + fences stay on the CPU per-draw path** — a large, high-count slice of the world. Bringing them
onto the GPU-driven path is higher value than optimizing terrain rendering. Good news: the per-draw wgpu path
*already conforms in the vertex shader* (it doesn't rewrite meshes on the CPU), so this is a **shader/data
port, not a new algorithm**.

### 11.1 How conform works today (per-draw path)
`shader3d.wgsl vs_main` conforms per instance, driven by `objects[i].conform0/1/2` (published by the CPU in
`EngineWgpu.cpp:1495-1511`) + a per-vertex `@location(5) conform_sel` (baked into the mesh at byte 32):
- **Mode 2 — individual ClipLand veg/fences** (the excluded set): purely per-vertex. `abs_xz = world_pos.xz +
  cam_pos`; `sy = surface_y(abs_xz)` (the shared `conform` module, group(4) heightmap). `conform_sel`: `1`
  ClipLandKeep → `y = sy + world_pos.y − bcSurfaceY`; `2` ClipLandOn → `y = sy − cam.y`; `0` rigid. Normals
  tilt by `surface_grad`. **Per-instance data = just `bcSurfaceY` (= `conform0.x`) + the mode flag**; the
  plane fields are unused.
- **Mode 1 — ForestPlain** (per-object bilinear plane): whole object conforms to a published plane
  (`conform0/1/2` = 10 floats). No per-vertex selector, no heightmap sample. *Separate, smaller follow-up.*

The heightmap group(4) is `self.conform.bind` in `mod.rs` (already bound by `draw_one`/`draw_indirect`/shadow).
The shadow pass conforms ClipLand independently (`shadow_depth.wgsl`), and shadow casters are unaffected by the
`GpuDrivenObject` colour suppression — so **shadows already work for these objects and stay correct**.

### 11.2 Plan — mode 2 (ClipLand) first
1. **Shader (`gpu_driven.wgsl`):** `#import conform::{surface_y, surface_grad}`; add the group(4) heightmap
   binding; add `@location(5) conform_sel: u32` to `vs_gpu`; port `vs_main`'s mode-2 block (read the mode +
   `bcSurfaceY` from the instance). VS-only ⇒ **both `fs_gpu` and `fs_gpu_prepass` inherit it for free**, so
   the prepass depth/normals stay consistent (the prepass shares `vs_gpu`).
2. **Instance data (`InstanceGpu`):** carry the mode + `bcSurfaceY` without growing the struct — set a
   `CONFORM_CLIPLAND` bit in the existing `flags`, store `bcSurfaceY` in a currently-`_pad` word (bitcast f32).
   No layout-size change; FFI `WgrInstance` mirrors it.
3. **Pipelines:** add group(4) to the GPU-driven colour + prepass pipeline layouts (`build_gpu_pipeline`); bind
   `self.conform.bind` at group 4 in `draw_gpu_driven_impl` (covers both draws).
4. **C++ (`RegisterGpuModel` / `BuildGpuInstance`):** drop the `ClipLandKeep|ClipLandOn` rejection; keep the
   opaque/cutout + non-transparent gate (cutout foliage = variant 1, already handled). In `BuildGpuInstance`
   set the conform flag when the shape is ClipLand and fill `bcSurfaceY`. **Decision: store the CPU-computed
   `bcSurfaceY`** (`GLandscape->SurfaceY(objectPos)`, matching `GCurrentConformPlane.bcSurfaceY`) once at
   add/update — it's static per static object and guarantees exact parity with the per-draw path; computing it
   in-shader from the instance origin is a possible later simplification but risks an xz-reference mismatch.
5. **Verify:** fences/individual trees conform to slopes identically to the CPU path; no cracks vs the terrain;
   prepass depth matches (no z-fight halo); the CPU per-draw path no longer draws them (double-draw check).

### 11.3 Open items / sequencing
- ~~**ForestPlain (mode 1)**~~ **✅ PORTED + user-verified (2026-07-09).** The interim
  `dyn_cast<ForestPlain>` exclusion is gone: `InstanceGpu`/`WgrInstance` grew 96→144 B carrying
  `conform0/1/2` (the full mode-1 plane, `conform2.z` = mode), `vs_gpu` runs the verbatim `vs_main`
  mode-1 block, and instances read `ForestPlain::GpuConformPlane()` (the already-cached
  `_conformPlane`; skewed t1/t2 squares bake conform into the transform and register rigid).
- **Interaction with instancing collapse (§3.6):** conform is per-vertex/per-instance and rides the instance
  record, so it composes with the collapse unchanged (the record already carries the instance id).
- Both modes landed and verified in-game (see the Implementation-status block for the final architecture,
  the `SaveOriginalPos` requirement, and the terrain-subdivision re-seat hook that keeps conform instances'
  transforms + `bcSurfaceY` correct across heightfield changes).

## 12. Partial suppression: buildings (proxy-bearing + mixed transparent/decal shapes) — priority, expands coverage

**Status:** 12a + 12d-core IMPLEMENTED + user-verified in-game (2026-07-11): 12a "no render problems"; 12d-core
"furniture renders as expected." 12d-full IMPLEMENTED (2026-07-11, uncommitted, pending in-game verify) —
proxies-only-no-complement buildings with all proxies GPU-eligible now downgrade to `Full` (CPU `Object::Draw`
skipped entirely). **12b is COMPLETE (2026-07-11) with no new code** — the mixed transparent/decal *admission*
already landed in 12a's per-section ownership (opaque sections → GPU, blend/decal complement → CPU passes),
and the §12.5 LOD-parity concern is resolved inherently (the GPU cull runs the same `FindSqrtLevel` formula on
the CPU's own per-frame LOD params, so owned + complement pick the same LOD — no forced_lod, no CPU coupling;
see §12.5). **All §12 coverage now landed.** No new GPU features used anywhere in §12.

**12d-full landed (`EngineWgpu.*`):** `RegisterGpuModel` records complement-bearing shapes in
`_gpuModelComplement`; `EmitGpuProxies` now returns whether **every** proxy moved to the GPU;
`SceneObjectCreated` upgrades a `Partial` instance to `Full` iff `allProxiesGpu && shape has no complement`.
A `Full` building's `Object::Draw` is skipped by `DrawSortObject` — output is identical to 12d-core (its CPU
draw already rendered nothing visible), this just removes the per-building `Animate`/lights/clip/DrawProxies
overhead. The complement guard is load-bearing: a glass building (or any complement-only shape) stays
`Partial` so its CPU-drawn glass/decals never vanish.

**⚠️ vtable regression (fixed 2026-07-11):** the first 12a/12d cut broke 3D UI — `GpuDrivenObject` was made
non-virtual and `GpuDrivenCoverage`/`GpuDrivenProxy` inserted mid-class, shifting every later `Engine` vtable
slot (ccache/PCH partial recompiles misdispatch). Fix: `GpuDrivenObject` kept at its ORIGINAL slot (now
delegates to `GpuDrivenCoverage`); the two new virtuals APPENDED at the true class end (with `SuppressWorldObjects`).
New `Engine` virtuals must always go at the class end — see the note there.

**12d-core landed (files, all in `EngineWgpu.*` + `Object.cpp`):** proxies (interior furniture) move onto
the GPU as **child instances**. `BuildGpuProxyInstance(world, model)` builds a rigid instance at the
COMPOSED transform `parentTransform × proxyLocalTransform`; `EmitGpuProxies` walks the finest graphical LOD
that carries proxies, registers each **Full-coverage** proxy shape (self-contained: no complement, no nested
proxies — anything Partial stays CPU) via `RegisterGpuModel`, and adds a child instance per eligible proxy,
recorded in `_gpuProxies : parent → {refLevel, [{proxyIndex, slot, model}]}`. `SceneObjectMoved` re-composes
children; `SceneObjectRemoved` + the destroy/shape-swap branch drop them (`RemoveGpuProxies`). New virtual
`Engine::GpuDrivenProxy(parent, level, proxyIndex)` (EngineWgpu-implemented) lets `Object::DrawProxies` skip
the GPU-handed proxies (only at `level == refLevel`) so furniture isn't drawn twice. Parent stays **Partial**
(still runs `Object::Draw` for any CPU-remaining proxies / complement); the Full-downgrade is 12d-full.

**12a landed (files):** shared ownership predicate `render::IsGpuOwnedSection` /
`IsGpuOwnedSectionSpec` (`BuildRenderPassDescriptor.hpp`) — the single source of truth used by the wgpu
`ClassifyGpuSection` (what the GPU takes) *and* `Shape::Draw`'s skip (what the CPU drops); tri-state
`GpuDrawCoverage{None,Full,Partial}` + `virtual GpuDrivenCoverage()` on the `Engine` base (with a
non-virtual `GpuDrivenObject` bool wrapper), overridden by `EngineWgpu`; transient
`GSkipGpuOwnedSections` (`Shape.hpp`/`ShapeDraw.cpp`) consulted in `Shape::Draw`'s T&L section loop;
`Object::DrawProxies` clears+restores the flag so proxy furniture draws in full; `RegisterGpuModel`
drops the whole-shape `NProxies>0` + any-non-owned-section rejections, registers owned sections per LOD,
and caches `Full`/`Partial` per model (`_gpuModelCoverage`), tagged onto each `GpuInstance`;
`DrawSortObject` returns early for `Full`, else runs `Object::Draw` under an RAII skip guard for `Partial`.

**Motivation.** The GPU-driven set is still *whole-shape-opaque, no-proxy* only. Two **whole-shape**
rejections in `RegisterGpuModel`/`ClassifyGpuSection` keep the single biggest remaining static slice —
**town buildings** — 100% on the CPU:
1. **Proxy-bearing shapes** (`s->NProxies() > 0` ⇒ ineligible, [EngineWgpu.cpp:1785](../EngineWgpu.cpp#L1785)).
   Buildings carry interior furniture as `_proxy[]` objects drawn *inline* by `Object::DrawProxies`
   ([Object.cpp:1009](../../Poseidon/World/Scene/Object.cpp#L1009)); the all-or-nothing CPU-draw
   suppression in `DrawSortObject` ([SceneDraw.cpp:1461](../../Poseidon/World/Scene/SceneDraw.cpp#L1461))
   would skip `DrawProxies`, so the furniture would vanish.
2. **Any non-opaque section** ⇒ whole shape ineligible (`ClassifyGpuSection` returns false for
   `blend != Opaque || surface != Default`, [EngineWgpu.cpp:1607](../EngineWgpu.cpp#L1607), and one false
   fails the whole model). One glass pane (blend) or one painted-on decal sends the entire building to CPU.

Both are unlocked by the **same keystone: make CPU-draw suppression *partial* instead of whole-object.**
The GPU takes the opaque-`Default` sections; the CPU keeps drawing only **the complement** — proxies + any
blend/decal sections — at that object's CPU-selected LOD, in the passes those already run in.

### 12.1 The tri-state: None / Full / Partial

`GpuDrivenObject(obj)` becomes tri-state (keep a `bool` convenience wrapper for the shadow-path call sites):
- **None** — not registered ⇒ CPU draws normally (today).
- **Full** — GPU owns *every* drawn section and the shape has **no proxies** ⇒ `DrawSortObject` skips the
  CPU draw entirely (today's `return`). Unchanged behaviour, unchanged code path.
- **Partial** — GPU owns the opaque-`Default` sections, but the shape **has proxies and/or ≥1 non-owned
  section** ⇒ `DrawSortObject` **still runs `obj->Draw`**, but the parent `Shape::Draw` skips the
  GPU-owned sections. `DrawProxies` runs (furniture draws); the complement sections draw in their normal
  passes (glass in `BlendOnly`, decals in the on-surface pass).

Coverage is decided per model at registration and cached alongside the model id (`GpuInstance` grows a
`coverage` field, or a parallel `_gpuPartial` set).

### 12.2 The section-skip mechanism (mirror `GSectionFilter` / `ConformPlane`)

The transient-global threading pattern already used for `GSectionFilter` and `GCurrentConformPlane`
([Shape.hpp:224-251](../../Poseidon/Graphics/Rendering/Shape/Shape.hpp#L224)) is exactly right — it threads
through `Object::Draw` → `Shape::Draw` (and proxies/sub-shapes) with **no** change to the many-overridden
`Object::Draw` signature:

- Add `extern bool GSkipGpuOwnedSections;` (default false). `DrawSortObject` sets it true around a
  **Partial** object's `obj->Draw` and restores it after (like the `PassKindHint` save/restore already
  there). Orthogonal to `GSectionFilter` — the pass filter still selects opaque-vs-blend; this *additionally*
  removes GPU-owned sections.
- In `Shape::Draw`'s section loop ([ShapeDraw.cpp:120-170](../../Poseidon/Graphics/Rendering/Shape/ShapeDraw.cpp#L120)),
  skip a section when `GSkipGpuOwnedSections && IsGpuOwnedSection(sec)` — **the same predicate**
  `ClassifyGpuSection` uses. Proxies are separate objects (their own `Object::Draw`); they reset the flag to
  false on entry (GPU never owns proxy geometry) so proxy interiors draw in full.

**Single source of truth (load-bearing).** Factor the eligibility predicate into ONE
`bool IsGpuOwnedSection(const ShapeSection&)` (opaque blend + `Default` surface + not hidden) used by BOTH
`ClassifyGpuSection` (what the GPU takes) and the `Shape::Draw` skip (what the CPU drops). If they ever
diverge: a section owned by neither ⇒ **hole**; owned by both ⇒ **double-draw** (z-equal overdraw, or
z-fight for the decal/blend cases). This helper is the correctness anchor of the whole feature.

### 12.3 `RegisterGpuModel` changes ([EngineWgpu.cpp:1754](../EngineWgpu.cpp#L1754))

- **Drop the `NProxies() > 0` whole-shape rejection** ([:1785](../EngineWgpu.cpp#L1785)). Proxies no longer
  disqualify — they just force `coverage = Partial`.
- **Stop letting a non-owned section fail the whole model.** `ClassifyGpuSection` returning false for a
  section becomes "CPU owns this section" (⇒ `Partial`), not `eligible = false`. Register the owned sections;
  count the non-owned ones. A model with **zero** owned sections stays `WGR_INVALID_MODEL` (nothing to GPU-drive).
- Compute `coverage`: `Full` iff `NProxies()==0` on every level **and** every non-hidden section is owned;
  else `Partial`. Store it with the model.
- Hidden proxy-MARKER sections (`IsHidden|IsHiddenProxy`) stay skipped as today — neither owned nor complement.

### 12.4 Passes & correctness

- **Opaque pass** (`GSectionFilter = OpaqueAndCutout`): a Partial object's parent `Shape::Draw` now draws
  *nothing* GPU-owned (skip flag) — only its DrawProxies furniture renders here. No double-draw with the GPU
  indirect set (which already drew the opaque geometry into the same colour + prepass targets).
- **Blend pass** (`BlendOnly`): the building's glass draws normally (never GPU-owned; the skip flag is false
  here because Partial objects are only re-entered per pass through the normal `_drawMergers` walk — set the
  flag per `DrawSortObject` invocation, so it applies in whichever pass that call belongs to).
- **On-surface/decal pass**: painted-on decals draw normally (not owned).
- **Shadows: unaffected.** Casters are a separate CPU pass (`SceneShadowPass`) not gated by
  `GpuDrivenObject`; a Partial building still casts a full CPU shadow (its GPU-owned geometry is *not* yet a
  GPU shadow caster — that's the still-pending GPU-path shadow work). No double shadow, no missing shadow.
- **Non-T&L / whole-shape fallback** ([ShapeDraw.cpp:185-199](../../Poseidon/Graphics/Rendering/Shape/ShapeDraw.cpp#L185)):
  buildings take the hardware-T&L path (they have a `_buffer`), so the skip lives in the T&L section loop.
  Verify a Partial object never routes through `DrawWholeShapeInPass` (which can't section-split) — if one
  does, treat it as CPU-only (don't register), since the whole-shape draw can't drop GPU-owned faces.

### 12.5 LOD parity on a Partial object — RESOLVED (already inherent, 2026-07-11)

Concern: the GPU cull+LOD compute picks the LOD for the **owned** geometry; the CPU picks `oi->drawLOD` for
the **complement**. If they differ ⇒ glass from one LOD over walls from another, or z-fight.

**Resolution: they already agree, by pure GPU-side computation — no fix needed, no CPU coupling.** The GPU
cull's LOD select ([cull.wgsl:160-177](../rust/src/gfx3d/cull.wgsl#L160)) is the SAME `FindSqrtLevel` /
`LevelFromDistance2` formula the CPU runs (`resol2 = dist²·lod_inv_width²·lod_scale²`, same near-clamp, same
sub-pixel cull, same ascending-resolution walk), and it is fed the CPU's own LOD params — `lod_scale =
Camera::Left()`, `lod_inv_width = _lodInvWidth` — pushed **once globally per frame** (`wgr_set_cull_params`),
not per object. So each side independently computes the same LOD from the same distance + same params and
lands on the same level. This is exactly the design constraint (per the user): *same LOD via pure GPU-side
logic, no per-frame per-object CPU communication, no added cull cost.*

- **(a) forced_lod (rejected).** Carrying a per-instance `forced_lod` pushed each frame from `oi->drawLOD`
  would add exactly the per-frame CPU↔instance coupling the GPU-driven epic exists to remove — and buys
  nothing over the shared-formula parity above. **Do not build it.**
- **Residual:** float-precision could flip the LOD by one at a transition boundary (rare, transient). If it
  ever visibly manifests, the fix stays pure GPU-side — bit-match the GPU formula to the CPU's — never CPU
  coupling. The `_lodInvWidth` adaptive feedback is fixed on the GPU (memory: fed per frame, not looped), a
  possible ≤1-frame lag; invisible in practice.

### 12.6 Staging

- **12a — Partial infra + proxy-bearing, fully-opaque-own-geometry shapes.** Add the tri-state, the
  `GSkipGpuOwnedSections` flag + `Shape::Draw` skip, the shared `IsGpuOwnedSection` predicate, and drop the
  `NProxies` rejection. Complement here = *proxies only* (own geometry fully owned ⇒ parent draws nothing,
  furniture draws). Proves partial suppression + DrawProxies coexistence with the smallest surface. Biggest
  single win (proxy buildings are common).
- **12b — Mixed transparent/decal shapes. ✅ DONE (2026-07-11, no new code).** The per-section CPU-ownership
  this needed already landed in 12a (`RegisterGpuModel` registers owned sections + tallies `hasComplement`),
  so glass/decal buildings already draw opaque→GPU, blend→sorted blend pass (CPU), decal→on-surface pass
  (CPU). §12.5 LOD parity is inherent (shared-input cull formula), forced_lod rejected. Nothing to build.
- **12c (follow-on, not this plan):** GPU-path shadow casting for Partial objects' owned geometry, once
  GPU-path shadows land generally — until then Partial buildings cast full CPU shadows (correct, just not yet
  GPU-driven).
- **12d — Proxies (furniture) → GPU child instances.** Layers additively on 12a's hooks; see §12.7. Turns a
  no-glass proxy building **Full** (zero CPU draw).

### 12.7 Proxies (furniture) on the GPU — §12d detail

A proxy is *already a static instance*: `DrawProxies` draws `world = parentTransform × proxy.obj->Transform()`
([Object.cpp:884](../../Poseidon/World/Scene/Object.cpp#L884)), and `NewProxyObject` builds a plain
`NewObject(shape,-1)` static ([ObjectClasses.cpp:963](../../Poseidon/World/Scene/ObjectClasses.cpp#L963)) — no
simulation. For a **static** parent both factors are static ⇒ the composed transform is a static GPU instance,
exactly `BuildGpuInstance`'s input. Shared furniture shapes instance for free.

**Mechanism (additive on 12a):**
1. **Emit child instances.** In `SceneObjectCreated`, after registering the parent, walk
   `shape->LevelOpaque(0)->Proxy(i)` (the richest LOD's proxy list, §caveat below); for each,
   `RegisterGpuModel(proxy.obj->GetShape())` (recursion-safe — the function already memoizes per shape) and
   `wgr_instance_add` one instance with `world = parentTransform × proxy.obj->Transform()` and the proxy's own
   conform (proxies are rigid furniture ⇒ mode 0).
2. **Child lifetime.** `GpuInstance` (or a side map `_gpuProxies : Object* → std::vector<uint32_t> slots`)
   tracks the child slots per parent. `SceneObjectMoved` re-composes + `wgr_instance_update`s each child;
   `SceneObjectRemoved` `wgr_instance_remove`s them. Reuses the existing hooks — no new engine plumbing.
3. **Fallbacks stay Partial.** A proxy whose shape is ineligible (transparent, itself proxy-bearing, animated)
   is NOT emitted → it stays in the CPU complement, and `DrawProxies` must still draw *that* proxy. So the
   parent's `GSkipGpuOwnedSections` draw needs a companion **per-proxy skip**: `DrawProxies` skips proxy `i`
   iff it was handed to the GPU. Simplest: the child-slot map keys by (parent, proxy index); `DrawProxies`
   consults it. If ALL proxies moved and own geometry is fully owned ⇒ parent is **Full** (skip CPU entirely).

**Caveats / gates (verify):**
- **LOD coupling (the real behavioral change).** Proxies are per-parent-LOD (furniture usually only on LOD0);
  independent GPU instances decouple that — furniture visibility becomes each proxy's *own* distance/sub-pixel
  cull, not the parent's LOD. For small furniture ~equivalent (self-culls at range), arguably cleaner. This is
  the proxy analogue of §12.5; accept + eyeball (option A).
- **Animated-selection proxies** (a proxy bolted to a moving part — windmill radar, door): NOT static even on
  a static parent; repositioned in *derived* `Object::Draw` overrides, not the base static path. Gate them
  out — only emit proxies of parents drawn by the base static `Object::Draw` (and skip any proxy whose
  `selection` maps to an animated selection). Leave those on CPU.
- **Instance-count growth** (N buildings × M furniture) is the point; §3.6 instancing collapse amortizes the
  extra sub-draws. Watch the instance-buffer / arg-buffer sizing (`DEFAULT_VARIANT_CAPACITY`).
- **Recursion**: `RegisterGpuModel` on a proxy shape that itself has proxies rejects (its own `NProxies>0`
  path under 12a) → that proxy stays CPU. Nested-proxy furniture is rare; not worth recursing initially.

**12d-core implementation notes (2026-07-11):**
- **Eligibility = `coverage == Full`** (reuses the §12a enum, no `RegisterGpuModel` change): `Full` already
  means "no complement sections **and** no proxies," i.e. a fully self-contained furniture mesh the GPU can
  draw whole. A `Partial` proxy (transparent bits, or its own nested proxies) stays on the CPU.
- **Reference-LOD double-draw window (accepted, option A).** Proxies are registered from ONE reference LOD
  (finest graphical level with proxies); `DrawProxies` skips GPU'd proxies only when drawing at that LOD.
  The GPU child instances are always resident and distance-cull independently. If a *coarser* LOD also lists
  furniture proxies AND the child instances haven't distance-culled yet, the coarser LOD's CPU proxies and
  the reference-LOD GPU children could briefly co-draw (double furniture). Uncommon (furniture is normally
  on the finest LOD only) and within the accepted LOD-coupling caveat; revisit with 12d-full if it shows.
- **Drift safety:** proxy children ride the parent's `SceneObjectMoved` (re-composed there) and the parent's
  drift tripwire in `GpuDrivenCoverage` (which calls `SceneObjectMoved`), so an unhooked parent mover
  refreshes furniture too. There is no separate per-proxy tripwire.
- **12d-full — IMPLEMENTED (2026-07-11).** Downgrade a proxies-only building (owned geometry fully GPU +
  every proxy GPU-eligible + no complement) to `Full` so its `Object::Draw` is skipped entirely. Realized via
  `_gpuModelComplement` (tracks "Partial had a complement" → never downgrade) + `EmitGpuProxies` returning
  `allProxiesGpu`; downgrade iff `allProxiesGpu && !hasComplement`. Removes the per-building
  `Animate`/lights/clip/DrawProxies overhead (NOT the Pass1 `SortObject` walk — that's the separate CPU-render
  divert). Note this also sidesteps the reference-LOD double-draw window (a `Full` building never CPU-draws
  proxies at all).

**Verify:** furniture still draws inside proxy buildings; no double-draw halo/z-fight on building walls
(prepass + colour); glass/decals still composite correctly; the `DrawSortObject` drift tripwire stays quiet;
CPU opaque-pass draw-call count drops by the building count; toggling `WGR_GPU_DRIVEN` off restores the exact
CPU image (A/B).
