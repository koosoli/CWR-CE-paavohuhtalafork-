# Plan: Procedural water rendering (coast, waves, transparency, refraction, planar reflection)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** REVISED (2026-07-11). Stage 1 (waves + glint) LANDED; the depth prepass, GPU-driven
multi-view cull, and MSAA prerequisites have all LANDED — this revision re-sequences the remaining
stages around what they actually expose and promotes **coast look** to the front.
**Roadmap slot:** Phase 3 (now unblocked) + Phase 5 (planar reflection). See
[implementation-roadmap.md](implementation-roadmap.md).

> The **look** half of the water rework. Builds on the flat GPU CDLOD water surface from
> [water-cdlod-geometry-plan.md](water-cdlod-geometry-plan.md) and the Stage-1 waved surface already
> shipped, turning it into parametric water with a **soft, alive coastline**, depth-based colour,
> transparency, screen-space refraction, and planar reflections — coupled to the procedural sky
> ([procedural-sky-plan.md](procedural-sky-plan.md)) and tonemapped by the HDR pipeline
> ([hdr-pipeline-plan.md](hdr-pipeline-plan.md)).
>
> **Prime directive: zero gameplay impact.** Waves and swash are cosmetic vertex/shader effects only;
> buoyancy and all sea-level reads stay on the flat CPU plane in `Landscape` (geometry plan §5). Waves
> stay gentle so floating objects never visibly detach; the cosmetic **swash** (waterline creeping up the
> beach) is a look-only reach that the simulation never sees.

---

## 0. Where we are today (verified 2026-07-11)

### 0.1 What Stage 1 shipped (the current water)

`engine/WgpuRenderer/rust/src/water/` (`mod.rs` + `water.wgsl`) draws a flat CDLOD grid at the frame's
sea level, displaced by a sum of **4 Gerstner waves in the VS**, shaded per-fragment with an **analytic
wave normal** ([water.wgsl:104](../rust/src/water/water.wgsl#L104)), a **sharp HDR sun glint** (blooms via
the existing bloom pass), a **Fresnel mix toward the fog/horizon tint** as a cheap reflection stand-in
([water.wgsl:218](../rust/src/water/water.wgsl#L218)), a distance **fade** that flattens far waves to kill
moiré, CSM + terrain-heightfield **sun shadowing**, aerial fog, and a **Fresnel-raised alpha**
([water.wgsl:249](../rust/src/water/water.wgsl#L249)). Look params are live-tuned by the **Water ImGui
tab** through `WgrWaterParams`. Draw is instanced per node, camera-relative, reversed-Z, depth-write off,
`GreaterEqual` so coastlines occlude water ([mod.rs:164-170](../rust/src/water/mod.rs#L164)).

**What Stage 1 does NOT do:** no depth-based colour, no transparency-over-seabed, no refraction, no real
reflection, and — the subject of this revision — **no coastline treatment at all**. The coast is purely a
**depth-test occlusion**: the flat blue surface is clipped by the already-drawn beach, giving a hard line
and a dry beach. MSAA fixed the z-fighting on that line but the result is visually flat.

### 0.2 The three prerequisites all landed — and the exact gap each leaves

The plan was blocked on the depth prepass, GPU-driven multi-view cull, and MSAA. All three are in. Each
now exposes *most* of what water needs but leaves one specific, small gap:

1. **Sampleable opaque depth — EXISTS, must be exposed to water.** The opaque depth+normal prepass runs
   over the first segment; the depth target is `Depth24PlusStencil8` allocated with `TEXTURE_BINDING` and
   a `DepthOnly` aspect view ([gfx3d/mod.rs:2289](../rust/src/gfx3d/mod.rs#L2289),
   [:2307](../rust/src/gfx3d/mod.rs#L2307)). Under MSAA a **conservative min-resolve** (a `max` reduction
   under reversed-Z: farther = smaller = conservative) reduces it to single-sample `Depth32Float`
   `wgr_3d_depth_resolved` ([depth_resolve.wgsl:25](../rust/src/gfx3d/depth_resolve.wgsl#L25),
   `RESOLVED_DEPTH_FORMAT` [gfx3d/mod.rs:40](../rust/src/gfx3d/mod.rs#L40)); `depth_sample_view`
   ([gfx3d/mod.rs:1152](../rust/src/gfx3d/mod.rs#L1152)) points at the resolved texture under MSAA and the
   raw depth aspect at 1×, so **water samples one binding regardless of sample count**. **Gap:** it is
   currently consumed only internally by `build_hiz` ([:4023](../rust/src/gfx3d/mod.rs#L4023)) — no public
   accessor, bound into no water-visible group. Stage 2 only has to **route `depth_sample_view` into the
   water pipeline's group(1)**. This is the small unlock that makes coast look possible.
2. **MSAA — water already draws into the multisampled scene target**, after all opaque (GPU-driven +
   CPU + terrain) is composited and **before** the resolve ([lib.rs:1239](../rust/src/lib.rs#L1239), target
   = MSAA `hdr` [lib.rs:1197](../rust/src/lib.rs#L1197)). Good for depth/coast work as-is; it is what makes
   the Stage-3 mid-frame colour resolve feasible (§0.3).
3. **Multi-view cull — EXISTS and is genuinely multi-view, but has no clip plane.** `frustum_planes(view_proj)`
   extracts 6 Gribb–Hartmann planes from *any* matrix ([cull.rs:142](../rust/src/gfx3d/cull.rs#L142));
   shadow cascades already reuse the whole cull/indirect path with their own light-VP
   ([`params_from_shadow_cascade`, cull.rs:1166](../rust/src/gfx3d/cull.rs#L1166)) and per-view records
   (`set_shadow_view_count` [cull.rs:621](../rust/src/gfx3d/cull.rs#L621)). A mirrored reflection camera is
   "just another view." **Gap:** `CullParamsGpu` ([cull.rs:18](../rust/src/gfx3d/cull.rs#L18)) has **no
   clip/oblique-near plane** — only the 6 frustum planes (far slot a no-op). Stage 4b's waterline clip must
   be *added* (an oblique near-plane on the mirror projection, an extra cull plane, or an FS-clip in the
   reflected pass), not merely reused.

### 0.3 Two capabilities that still don't exist (real new work)

- **A pre-water scene-colour copy for refraction.** Nothing exposes the in-progress scene colour
  mid-frame: the MSAA `hdr` target is `RENDER_ATTACHMENT`-only ([lib.rs:556](../rust/src/lib.rs#L556)) and
  the one resolved copy (`hdr_resolve`, [lib.rs:581](../rust/src/lib.rs#L581)) is produced *after* water at
  the tonemap seam ([lib.rs:1288](../rust/src/lib.rs#L1288)). Refraction (Stage 3) needs a **new pre-water
  resolve/blit** of the opaque scene into a sampleable target — under MSAA a mid-frame resolve. Owned by
  Stage 3, not by coast.
- **An importable sky module.** `sky/sky.wgsl` has no `#define_import_path` and its LUTs live in a private
  group. Sky reflection (Stage 4a) needs a `sky_radiance(dir)` the water FS can call.

### 0.4 New capability the plan didn't anticipate

A **view-space normal G-buffer** (`Rg16Float` octahedral, `wgr_3d_normal`
[gfx3d/mod.rs:2330](../rust/src/gfx3d/mod.rs#L2330), `normal_view()`) is now written in the prepass,
sampled by nothing yet. Water doesn't need it directly; it's what later makes SSAO — which quietly firms
up coast crevices and wet rock — cheap. Out of scope here, noted as an adjacency.

### 0.5 Coast-relevant terrain facts (for the terrain side of coast look)

The seabed is just terrain below sea level (depth encoded as negative terrain height — geometry plan
§0). The terrain FS already reconstructs per-fragment **world height** (`world_y = world_pos.y +
cam_pos.y`, [terrain.wgsl:257](../rust/src/terrain/terrain.wgsl#L257)) and a per-fragment **world normal**
(`sample_normal`, [terrain.wgsl:78](../rust/src/terrain/terrain.wgsl#L78),
[:240](../rust/src/terrain/terrain.wgsl#L240)), and blends a bindless ground-texture array before lighting
([terrain.wgsl:227](../rust/src/terrain/terrain.wgsl#L227)). It has **no `sea_level`** — that value lives
only on the water path (`WaterParams.sea_level`). Adding `sea_level` (+ a shared swash phase) to
`TerrainParams`/`WgrTerrainParams` ([terrain.wgsl:16](../rust/src/terrain/terrain.wgsl#L16),
[ffi.rs:515](../rust/src/ffi.rs#L515)) is the single prerequisite for the terrain side of coast look.

---

## 1. Design decisions

1. **Coast look is a joint water+terrain effect keyed on height-above-sea, sharing one swash phase.**
   Both surfaces read the *same* `sea_level` and the *same* cosmetic **swash** oscillation, so the water's
   transparent edge, the foam line, and the terrain's wet high-water mark all register at one moving
   waterline. This shared key is the organizing idea; §3's coast stage builds both sides against it.

2. **Gerstner-sum waves in the VS, analytic normals — FFT deferred.** (Unchanged, shipped.) Low-frequency
   shape in the VS; high-frequency normal detail per-fragment. A Tessendorf FFT ocean is a later optional
   upgrade; the game's seas are calm coastal water where Gerstner is plenty.

3. **Keep waves gentle and cosmetic; swash is a look-only reach.** Wave amplitudes ~`maxWave`
   (~0.1–0.3 m) so crests never separate a hull from the flat buoyancy plane. The gameplay tide bob is only
   ±0.25 m — too small to read as surf — so the **swash** (waterline advancing/retreating along the beach
   slope) is an additional *cosmetic* excursion (order ±0.5–1.5 m of horizontal travel), driven purely by
   the wave-time uniform and never read back into simulation.

4. **Depth-based colour + Beer-Lambert clarity + soft shoreline.** Reconstruct seabed world position from
   sampled opaque depth; `water_depth = surface_y − seabed_y`. Blend `shallow → deep` colour, attenuate by
   `exp(−extinction · water_depth)` (per-channel = the clarity/turbidity control), and fade alpha → 0 as
   `water_depth → 0` so the shoreline **dissolves over the visible wet beach** instead of hard-clipping.
   This is the coast keystone and needs only the depth exposure (§0.2.1).

5. **Fresnel mix of reflection and refraction.** Schlick Fresnel on `dot(view, normal)` (F0 ≈ 0.02):
   grazing → mostly reflection (sky/scene), top-down → mostly refraction. Sun **specular glint** (shipped)
   replaces the legacy specular texture.

6. **Screen-space refraction from a pre-water scene-colour copy.** Resolve/blit the opaque scene into a
   sampleable `refraction_src` right before water; sample it at the screen UV perturbed by the water normal
   (scaled by `1/depth`), clamped by the depth texture so it never samples a pixel *nearer* than the water
   (no foreground bleed). New infra (§0.3); Stage 3.

7. **Planar reflection is exact for a flat plane — stage it, and it's now much cheaper.** Reflect the view
   ray for sky (4a); a mirrored-camera half-res re-render for scene (4b). 4b is **≈ one extra cull dispatch
   + one indirect draw** through the now-landed multi-view cull path (the mirror is "just another view",
   proven by shadow cascades) — no scene re-walk. **The remaining cost is (a) the raster/shade of the
   reflected view and (b) the missing waterline clip** (§0.2.3): add an oblique near-plane / extra cull
   plane so below-water instances are rejected, or FS-clip in the reflected pass. Mitigate raster with
   half-res, reduced draw distance, opaque-only, on-screen gating.

8. **Sky-coupled per-map look.** `WgrWaterLook`/`WaterSettings` carry authored per-map fields (deep/shallow
   colour, extinction, F0, glint, wave set, **coast band width, foam, swash amplitude/speed, wet-band
   depth/darkening**). Couple deep/ambient/reflection tint to the procedural sky's time-of-day
   (`frame.sun_ambient`/`sun_diffuse`, already atmosphere-derived when `sun_diffuse.w > 0.5`) so night
   water darkens and sunset water reddens. Exposed through the existing **Water** ImGui tab.

9. **HDR/linear + fog correctness.** `linear` override on `Rgba16Float`; write un-clamped radiance (glint
   blooms); apply the froxel `apply_fog` so distant water melts into the horizon.

10. **wgpu-only, flagged, GL33 untouched.** Keep sub-toggles (coast / refraction / reflection) for bring-up
    and A/B.

---

## 2. Data + FFI additions

- **Expose the opaque depth to water (Rust-internal, no FFI).** Add a public accessor for
  `depth_sample_view` and bind it (plus a comparison/non-filtering sampler) as a new entry in the water
  pipeline's group(1) ([mod.rs:39-51,128-132](../rust/src/water/mod.rs#L39)). One binding covers 1× and
  MSAA (the view already indirects to the resolved copy under MSAA, §0.2.1). The water pass runs after the
  resolve is recorded, so the resolved depth is valid when water samples it (verify op ordering:
  resolve is inside `build_hiz` [gfx3d/mod.rs:4029](../rust/src/gfx3d/mod.rs#L4029), before the colour
  sub-pass).
- **`sea_level` (+ swash phase) into `TerrainParams`.** Extend `TerrainParams`/`WgrTerrainParams`
  ([terrain.wgsl:16](../rust/src/terrain/terrain.wgsl#L16), [ffi.rs:515](../rust/src/ffi.rs#L515)) with
  `sea_level` and a `swash` scalar (or fold both into the shared `Frame` UBO if we'd rather one value serve
  water + terrain + lit meshes — `Frame`/`FrameParams` [frame.wgsl:8-37](../rust/src/shaders/frame.wgsl#L8)
  have no sea field today). C++ pushes the same `sea_level`/swash it already computes for water so the two
  sides stay locked.
- **`WgrWaterLook` fields** (declare in [wgpu_renderer.hpp](../include/wgpu_renderer.hpp) + [ffi.rs](../rust/src/ffi.rs)
  with a size `static_assert`, like `WgrSky` [ffi.rs:274](../rust/src/ffi.rs#L274)): existing wave/glint
  fields plus **deep/shallow colour, extinction (vec3), F0, coast_fade_dist, foam_width/intensity,
  swash_amp/speed, wet_band_height/darken/gloss**. Most already flow through `WgrWaterParams`; extend it,
  keep the size assert.
- **Refraction bindings (Stage 3, Rust-internal):** a `refraction_src` HDR texture + a pre-water resolve of
  the MSAA scene, bound into group(1). No FFI.
- **C++ `Engine::WaterSettings`** already exists for the Water tab; extend with the coast/refraction/reflection
  fields and per-ToD/per-map presets, mirroring `SkySettings`.

---

## 3. Rendering stages (re-sequenced: coast before refraction/reflection)

Coast look is promoted ahead of refraction and reflection because it is the cheapest (depth already
resolved; +1 terrain uniform), the most visible improvement, and needs neither the scene-colour copy nor
the mirror pass. Each stage is independently shippable behind the water flag with per-effect sub-toggles.

### Stage 2 — Depth-based colour + transparency + **soft shoreline** (exposes depth to water)
- Route `depth_sample_view` into water's group(1); reconstruct seabed depth (reversed-Z aware, against the
  actual projection — do not copy conventional-depth math); `water_depth` → shallow/deep colour blend +
  Beer-Lambert clarity; alpha fades to transparent as `water_depth → 0` so the shoreline dissolves over the
  visible wet beach; enable `Alpha` blend (already on).
- **Exit:** shallows turn turquoise, depths dark, and the waterline is a **soft transparent fade over the
  seabed** — the hard depth-clip line is gone. Clarity/colour tunable in the Water tab.

#### Stage 2 — concrete implementation notes (design resolved 2026-07-11)

Verified against the current renderer; these are the non-obvious decisions the code makes real.

1. **Reconstruct the seabed with a full inverse-VP unproject, not hand-linearised depth.** `frame.proj` is
   a **forward infinite-far** perspective (`ConvertProjectionMatrix` + `_33=1, _43=-cNear`,
   [EngineWgpu.cpp:1281-1293](../EngineWgpu.cpp#L1281)); every 3D pipeline applies `reverse_z` in-shader
   ([frame.wgsl:149](../rust/src/shaders/frame.wgsl#L149)), so the depth buffer holds `d = 1 −
   forward_ndc_z`. The proj is reversed-Z/infinite-far and ill-conditioned to invert in f32 (the sky hit
   exactly this, [lib.rs:867-878](../rust/src/lib.rs#L867)). So add **`inv_view_proj = inverse(view) *
   inverse(proj)`** to the shared `Frame` UBO, computed **in Rust in f64, inverted separately** (mirroring
   the sky). The water FS then unprojects `vec4(ndc.xy, 1 − d, 1)` → camera-relative seabed, divide by w.
   - **No viewport needed.** The seabed shares the water fragment's view ray, so its `ndc.xy` = the
     surface's `ndc.xy`, recovered by re-projecting `in.world_pos` (`clip = proj·view·world_pos; ndc.xy =
     clip.xy/clip.w`). The depth *texel* is fetched with `textureLoad(depth, vec2<i32>(pos.xy), 0)` from
     `@builtin(position)` — the depth texture is framebuffer-resolution, so window pixel = texel.
   - `water_depth = in.world_pos.y − seabed_rel.y` (both camera-relative; `cam_pos.y` cancels).
2. **`inv_view_proj` lives in the shared `Frame` UBO, appended Rust-side — no C-ABI change.** `WgrCamera`
   (C ABI, 576 B) is cast directly into the camera UBO. Bump `CameraGroup::bind_size` to `sizeof(WgrCamera)
   + 64` ([gfx3d/mod.rs:444](../rust/src/gfx3d/mod.rs#L444) — flows to the layout `min_binding_size`,
   stride, and bind size), append `inv_view_proj: mat4x4<f32>` to WGSL `Frame`
   ([frame.wgsl:37](../rust/src/shaders/frame.wgsl#L37)), and in the upload loop
   ([gfx3d/mod.rs:3020](../rust/src/gfx3d/mod.rs#L3020)) write the computed matrix at `off + 576` after the
   `WgrCamera` bytes. Reusable by SSAO / refraction / contact shadows later. (576 is 16-aligned — mat4 ok.)
3. **Draw water in its own render pass with depth attached READ-ONLY.** The shared colour sub-pass binds
   depth *writable* (`depth_ops: Some`, [lib.rs:1205-1215](../rust/src/lib.rs#L1205)) and the GPU-driven
   colour pipeline writes depth ([cull.rs:1259](../rust/src/gfx3d/cull.rs#L1259)), so water cannot sample
   that depth texture inside that pass at 1×. Instead, **skip `Plan3dOp::Water` in `render_ops` and draw
   all water ops in a dedicated pass** opened right after the 3D sub-pass closes (before the 2D sub-pass),
   world segment only (`start == 0`): colour = `target` (Load), depth = `seg_depth` with **`depth_ops:
   None` + `stencil_ops: None`** (read-only). A read-only depth attachment may be sampled in the same pass
   — legal at 1× (same texture) and at MSAA (water samples the *separate* resolved `depth_sample_view`).
   Depth **test** vs the coast still works (GreaterEqual vs the prepass-laid depth); water writes no depth
   (unchanged). This isolates the read-only requirement to water and is the natural home for the Stage-3
   refraction blit (it slots in right before this pass).
   - Ordering shifts water to *after* the segment's opaque + CPU-blended draws. Opaque occluders (bridge
     decks) are in the depth prepass so water is still correctly occluded; transparent-over-water (glass on
     a pier) is a rare corner accepted for Stage 2.
4. **Depth binding lifetime.** `depth_sample_view` is rebuilt on resize ([gfx3d/mod.rs:2320](../rust/src/gfx3d/mod.rs#L2320)).
   Add a `Gfx3d::depth_sample_view()` accessor + a `depth_gen` counter bumped in `ensure_depth`; `Water`
   holds group(1) = {params UBO (binding 0), depth `texture_depth_2d` (binding 1)} and a
   `set_depth_view(device, view, gen)` that rebuilds group(1) only when `gen` changes. Seed with a 1×1
   dummy `Depth32Float` at construction so the pipeline/bind are valid before the first resize (water
   doesn't draw until `have_params`, by which point real depth exists; a stray dummy load returns 0 → far →
   deep water, harmless). One layout entry (`sample_type: Depth`, non-multisampled) serves both the 1×
   depth-aspect and the MSAA resolved `Depth32Float`.
5. **Increment split.** *Increment 1 (now):* infra (2–4) + FS reconstruction with **hard-coded**
   deep/shallow colour, extinction, and coast-fade distance → visible depth colour + soft shoreline, pure
   Rust/WGSL, no C++ churn. *Increment 2:* promote those to `WgrWaterParams` + the Water ImGui tab (the
   Stage-1 "hard-code then UBO" pattern).

### Stage 2c — **Coastline: foam + swash (water side) + wet band (terrain side)**  *(new; was old "foam" Stage 6)*
- **Water side:** foam mask where `water_depth` is in a thin near-zero band
  (`1 − smoothstep(0, foam_width, water_depth)`), procedural noise scrolling shoreward; add crest foam from
  Gerstner steepness/Jacobian as a bonus. A **swash** oscillation (`sin(2π·t·swash_speed + noise)`)
  modulates the foam-band centre and the alpha-fade threshold so the visible waterline creeps up and down
  the beach slope.
- **Terrain side:** with `sea_level` + swash now in `TerrainParams`, compute `height_above_sea = world_y −
  sea_level`; within a slope-gated band up to the **swash high-water** mark, darken + gloss-boost
  (optionally desaturate) the blended albedo *before* lighting ([after terrain.wgsl:232](../rust/src/terrain/terrain.wgsl#L232)),
  slope-gated by the existing `sample_normal` so cliffs stay dry. Optional thin foam line at the exact
  waterline, sharing the swash phase.
- **Coupling:** both sides key on the same `sea_level` + swash phase, so the wet high-water edge, the foam
  line, and the water's transparent edge register at one moving waterline.
- **Exit:** the coast reads as a living intertidal zone — wet, glossy sand under a foamy, gently
  advancing/retreating waterline — with no hard edge. All procedural; zero asset edits; zero gameplay
  impact.

### Stage 3 — Screen-space refraction — **ATTEMPTED then REVERTED (2026-07-11); needs a separate pass**
Screen-space refraction (sampling a pre-water copy of the *composited* scene at a wave-perturbed UV) was
built and backed out the same day. **Why it can't work here:** the composited frame contains the
first-person weapon and (third-person) the player model. A screen-space depth guard can't reliably
exclude them — they read as ugly ghosts refracted through the surface. **Refraction must render its own
underwater-only view** (seabed + submerged geometry, excluding the player/weapon/foreground actors) into a
dedicated target, then sample that. That is a Stage-4b-class effort (a second scene pass), and given OFP's
calm coastal water and the strong flat depth-tint from Stage 2, it is **deferred** — revisit only if a
proper underwater pass is built. The `refraction_src` copy, `WgrWaterParams.refract_*`, the water group(1)
colour binding, and the Water-tab controls were all removed; the flat depth-tinted body (Stage 2) stands.

### Stage 4 — Reflection (4a sky-only, 4b planar scene)
- **4a — DONE (2026-07-11), via a sky ENV MAP (not per-pixel `sky_radiance`).** The plan's "importable
  `sky_radiance(dir)` called per water fragment" was rejected on two counts: (1) every sky helper reads the
  `sky` group(0) globals, so an `#import` would collide with water's own group(0) (camera) — sharing would
  need every helper re-plumbed to take the LUTs/params as args; (2) a full atmosphere raymarch per water
  pixel is expensive. Instead: `sky_radiance(dir)` was extracted **inside `sky.wgsl`** (disc-free; `fs_sky`
  now = `sky_radiance` + disc + tonemap, output identical), and a new **`fs_sky_env`** bakes it into a
  256×128 **equirect env map** once per frame (`Sky::render_env`, reusing the sky group(0) bind). Water
  binds that one texture (group1 bindings 2/3) and, on the HDR path, reflects it: `refl =
  sky_env_sample(reflect(normalize(world_pos), n))`, Fresnel-mixed as before. This **supersedes the
  `fog_color`/`sun_up` reflection hack** (kept only as the LDR-direct fallback): night water now reflects a
  genuinely dark sky, and the pre-dawn horizon glow appears **only** on fragments that geometrically reflect
  toward it — no uniform pink wash. Sun disc excluded (per decision: analytic Blinn-Phong glint stays the
  glint source). Env map is disc-free linear radiance; equirect UV convention shared by `fs_sky_env` (bake)
  and `sky_env_sample` (water). **Not yet run in-game** (Rust+shader validated).
- **4b:** mirrored-camera half-res planar re-render through the **existing multi-view cull path** (mirror =
  another view, `frustum_planes(mirror_vp)`, its own `set_shadow_view_count`-style view + records). **Add a
  waterline clip** — the one missing piece (§0.2.3): an oblique near-plane on the mirror projection or an
  extra `CullParamsGpu` plane so below-water instances are rejected before draw, else FS-clip in the
  reflected pass. Flip winding; needs a reflected color+depth target + resolve. Composite over 4a where
  rays miss geometry.
- **Exit:** grazing water mirrors the sky and coastline; top-down water transmits.

### Stage 5 — Per-map look settings + Water tab + sky coupling
- `WgrWaterLook` fully wired incl. coast/foam/swash/wet-band fields; per-map/per-ToD presets; tie
  deep/ambient/reflection tint to `frame.sun_*`/froxel so colour tracks time of day and weather.
- **Exit:** per-map water + coast colour, clarity, and swash are authorable and live-tunable; night/sunset
  water looks right and consistent with the sky.

### Stage 6 — Underwater view (optional, later)
- Camera below `sea_level`: underwater fog/tint, from-below surface shade, caustics on the seabed (the
  view-space normal G-buffer and depth make screen-space caustics/AO tractable). Larger scope; follow-up.

---

## 4. Cost & correctness risks

- **Depth ordering (Stage 2).** Water must sample the depth *after* the resolve is recorded. The resolve is
  inside `build_hiz` ([gfx3d/mod.rs:4029](../rust/src/gfx3d/mod.rs#L4029)), which runs before the colour
  sub-pass ([lib.rs:1176](../rust/src/lib.rs#L1176)) where water draws — so ordering holds; keep it that way
  if the frame graph is touched. Reversed-Z: reconstruct seabed depth against the actual projection (min-
  resolve gives the *conservative farther* sample, fine for low-frequency water colour).
- **Swash must never touch simulation.** It is a shader-time excursion only; buoyancy/sea reads stay on the
  flat `_seaLevelWave` plane. Keep the swash amplitude a look-only knob; document it so nobody wires it into
  physics. Boats/wakes read `GetSeaLevel() − 0.1` ([Ship.cpp:825]) and stay on the flat plane.
- **Terrain wet band is render-only and must not disturb gameplay height.** It reads `sea_level` and
  recolours; it changes no geometry and no heightmap. Slope-gate it so cliffs and steep coast don't get a
  false tidal band.
- **Refraction colour copy (Stage 3).** A mid-frame MSAA resolve is the real cost; keep it opaque-only and
  behind the refraction sub-toggle. The resolved copy must target the correct segment's resources.
- **Planar reflection (Stage 4b) is the expensive item and needs the missing clip plane.** GPU-driven
  removes the *submission* half, not the *raster* half — budget the reflected view's raster (half-res,
  reduced set, opaque-only, on-screen gating). The reflected pass has no valid main-view froxel — build a
  reflected froxel or skip aerial fog in the reflection (acceptable). Default to 4a on lower tiers.
- **Sky module refactor (4a).** Making `sky.wgsl` importable must not regress the fullscreen sky pass; keep
  `fs_sky` and add a pure `sky_radiance(dir)` the pass and water share.

---

## 5. Landing order

Stage 1 (waves + glint) — **DONE**. Then, per the confirmed re-sequencing:

**Stage 2 (expose depth → depth colour + soft shoreline) → Stage 2c (coast: foam + swash + terrain wet
band) → ~~Stage 3 (refraction)~~ → Stage 4a (sky reflection) → Stage 4b (planar reflection) → Stage 5
(per-map look + tab + sky coupling)**, with Stage 6 (underwater) as optional later polish. One PR per
stage, behind the water flag with per-effect sub-toggles. **Stages 2 + 2c are DONE** (uncommitted
2026-07-11); **Stage 3 (screen-space refraction) was attempted and reverted** — it needs a dedicated
underwater pass (see above), deferred. Two coast-look bug-fixes also landed 2026-07-11: **foam is now lit**
(no night glow) and **water reconstructs seabed depth from a FARTHEST-sample MSAA resolve** so A2C foliage
/ rotor edges no longer ring with foam. Stage 4a (sky reflection) is the next look work — it needs
`sky.wgsl` refactored into an importable `sky_radiance(dir)` module (§0.3).
