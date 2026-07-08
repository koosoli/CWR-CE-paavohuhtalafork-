# Plan: Bindless object textures + samplers

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** IMPLEMENTED in the working tree (2026-07-08, uncommitted; pending user build + in-game
validation). The sampler-array half is new-on-this-device — see §4.
**Roadmap slot:** pulled forward out of Phase-4 GPU-driven work (§Stage 4 of
[rendering-performance-plan.md](rendering-performance-plan.md) /
[gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)) because it cuts per-draw bind churn for
**every** pass — colour, the new depth prepass, and the coming CSM-8 + planar-reflection passes.

---

## 0. Why (measured motivation)

The [depth prepass](depth-prepass-plan.md) landed and measured as ~pure overhead in a simple scene (no
overdraw to reject), dominated **not by vertex throughput but by per-draw fixed cost**: the prepass and
colour pass both rebind vbuf/ibuf, texture, sampler, and sometimes the pipeline between draws. Bindless
textures + samplers remove the texture/sampler rebinds outright and — because texture/sampler stop being
part of the instancing key — let far more same-mesh draws coalesce into one instanced draw. This helps
the colour pass, the prepass, and (crucially) multiplies across the many-pass future (CSM 4→8, planar
reflections). It is also the exact precursor the GPU-driven / `multi_draw_indirect` path needs.

The machinery already existed in-tree: terrain's ground layers are a bindless
`binding_array<texture_2d<f32>>` indexed non-uniformly per fragment, and the device already requests
`TEXTURE_BINDING_ARRAY` + `SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING`. This applies
the same pattern to object textures.

---

## 1. What landed

- **`SharedTextures` gains a bindless object-texture array** ([textures.rs](../rust/src/textures.rs)):
  one `binding_array<texture_2d<f32>>` (`bindless_layout` / `bindless_bind`) covering every live object
  texture. Each `Texture2D` gets a dense `slot: u32` (0 = the white fallback / holes); create allocates a
  slot (recycled free-list, else grow), destroy frees it. The bind group rebuilds **lazily** via
  `ensure_bindless` (called once per frame in `render_frame`) so level-load texture churn costs one
  rebuild per frame, not O(N²). Heterogeneous BC1/2/3/RGBA8 + mixed sizes are fine (a `binding_array`,
  unlike a `texture_2d_array`, holds mixed-format/size views — terrain already relies on this).
- **Bindless 8-variant sampler array** (`sampler_array_layout` / `sampler_array_bind`): the existing 8
  point×clampU×clampV sampler variants as one `binding_array<sampler, 8>`, bound once. The old
  single-sampler `sampler_binds` + `texture_bind`/`sampler_bind` stay — the shadow-depth and 2D passes
  still use them.
- **Per-instance index in the material array** ([shader3d.wgsl](../rust/src/gfx3d/shader3d.wgsl)):
  `prepare` packs `(tex_slot << 3) | sampler_idx` into the material's spare `emissive.w` (only
  `emissive.rgb` is read for shading). `fs_main` and `fs_prepass` unpack it and sample
  `textures[tex_slot]` with `samplers[sampler_idx]`. The material is read by BOTH the plain and skinned
  pipelines (bound at group(1)/binding(1) in each), so skinned characters index the bindless arrays too.
- **Object pipeline layout groups 2/3 are the bindless arrays** ([gfx3d/mod.rs](../rust/src/gfx3d/mod.rs));
  `draw_one` binds them **once per run** (tracked in `Pass3dState::bindless`), not per draw. Shadow
  pipelines keep their single-texture layouts.
- **`BucketKey` drops `texture_id` + `sampler`**: same-mesh draws with different textures/samplers now
  merge into one instanced draw — a draw-count reduction stacked on the bind-churn win.
- **Capacity** ([lib.rs](../rust/src/lib.rs)): `object_texture_cap = clamp(adapter limit, 512, 8192)`,
  requested as `max_binding_array_elements_per_shader_stage`. With `PARTIALLY_BOUND_BINDING_ARRAY` the
  bind carries only up to the high-water slot; otherwise it pads the tail to the cap with the white view.

---

## 2. Design decisions

1. **Index rides the material, not the object array.** The object (world/conform) storage is bound only
   for the *plain* pipeline (the skinned pipeline binds the bone palette at that slot). The material array
   is bound for both, so it is the only per-instance place a skinned fragment can read the texture index.
2. **One packed `u32` in `emissive.w`.** `(slot << 3) | (sampler & 7)`: sampler is 3 bits, leaving 29 for
   the slot — vastly more than the 8192 cap. `bitcast<u32>` recovers it; no arithmetic touches the field.
3. **Slot 0 = white.** Matches the old `texture_bind` fallback: a zero/missing handle samples white.
4. **Quad-uniform index ⇒ implicit-mip `textureSample` is legal.** Unlike terrain (per-fragment cell
   index → needs `textureSampleGrad`), the object index is per-*instance*, and a 2×2 derivative quad is
   always one primitive = one instance, so the index is quad-uniform. Plain `textureSample` is safe.
5. **Lazy rebuild.** Texture create/destroy just marks dirty; `ensure_bindless` rebuilds once per frame.

---

## 3. Not done / deferred

- **Shadow + 2D passes stay single-texture** (their own layouts). Making the shadow alpha-cutout path
  bindless is a later, separate step.
- **Overflow past the cap** samples white (logged-worthy; a level with >`object_texture_cap` unique
  textures is not expected in OFP content). If it ever bites, raise the cap or add an LRU eviction.
- **Indirect / GPU-driven** ([gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)) is the next
  consumer: with texture/sampler per-instance and geometry merged, the per-instance storage *is* the draw
  stream, so `multi_draw_indirect` becomes a small step and the prepass collapses to one more indirect call.

---

## 4. Load-bearing hazards

- **Sampler-array non-uniform indexing is new on this backend.** Terrain proves texture arrays; the 8-way
  sampler array is exercised for the first time here. It rides `TEXTURE_BINDING_ARRAY` (DX12/Vulkan/Metal
  per the wgpu docs) and validates in the CPU `entry_shaders_compose` test under the granted naga
  capabilities, but the first in-game run is the real proof. If a target GPU rejects it, the fallback is
  to keep the sampler as a per-draw group(3) bind (sampler variety is tiny) and re-add sampler to
  `BucketKey` — textures stay bindless regardless.
  - **Device limit gotcha (hit on first run):** BOTH binding-array limits **default to 0** even on devices
    that fully support binding arrays, and `create_bind_group_layout` panics ("limit is 0, count was N")
    until each is raised in `required_limits`: `max_binding_array_sampler_elements_per_shader_stage` (set to
    8, our sampler count) AND `max_binding_array_elements_per_shader_stage` (set to the object cap, which
    also covers terrain's 512). **Do not derive these from `adapter.limits()`** — it can report the 0
    default; the wgpu docs guarantee any array-capable device supports >= 500k resources / 1000 samplers,
    so request the fixed values you use (we gate on array features first). And wgpu counts the sampler
    array's 8 elements against the GENERAL `elements` limit too (not only the sampler limit), so the object
    pipeline layout needs `object_texture_cap + 8` there — request headroom above cap, not exactly cap.
- **`emissive.w` overwrite.** The shader only reads `emissive.rgb`, so packing the index into `.w` is
  safe — but any future use of `material.emissive.w` for shading would collide. Documented at the pack
  site and the WGSL binding.
- **Capacity vs `PARTIALLY_BOUND`.** Without the feature the startup bind pads to `object_cap` (up to
  8192) white views — fine (pointers), but keep the cap sane. Slot 0 must always be white.

---

## 5. Cross-references
- [depth-prepass-plan.md](depth-prepass-plan.md) — the measurement that motivated this; `fs_prepass` also
  samples bindless for the foliage cutout.
- [rendering-performance-plan.md](rendering-performance-plan.md) — bind-churn vs draw-count wins.
- [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) — the GPU-driven path this unblocks.
- [compute-skin-bake-plan.md](compute-skin-bake-plan.md) — the other per-pass-cost amortization (skinning).
