# Sky-visibility ambient plan (heightfield sky-view factor → ambient occlusion)

**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust). **Status:** PLAN (updated 2026-07-12, revised
for the landed SH sky-irradiance ambient + env-map water reflection).

> **RND-030 reconciliation (2026-08-02):** the status line above is out of date: terrain sky-visibility AO is **implemented** -- `WgrSkyVisibility` (`ffi.rs:594`) and `terrain_set_sky_visibility` (`ffi.rs:1899`).
>
> The status line is left as written rather than rewritten, so the document's own history stays readable. See [RND-030-renderer-plan-reconciliation-20260802.md](../../../docs/roadmap/decisions/RND-030-renderer-plan-reconciliation-20260802.md).

## 0. What landed since the first draft (read this first)

Two features this plan originally anticipated as *future* work have **landed**, changing the exact
insertion points (not the design):

- **Directional sky-irradiance ambient (SH-9).** The flat `sun_ambient` fill is gone on the sky-lit
  path. `sky_sh.wgsl` projects the sky env map into 9 RGB SH coefficients each frame; `frame::sky_irradiance(n)`
  ([`frame.wgsl:105`](../rust/src/shaders/frame.wgsl)) evaluates *directional* ambient per surface
  normal. Terrain ([`terrain.wgsl:305`](../rust/src/terrain/terrain.wgsl)) and objects
  ([`shading.wgsl:83`](../rust/src/shaders/shading.wgsl)) now use `sky_irradiance(n) * frame.sun_ambient.w`.
  The `sky_irradiance` doc comment already reserves the hook: *"A future sky-visibility (AO) term
  multiplies this per point by the fraction of sky it can see."* — **this plan is that term.**
- **Real env-map water reflection (Stage 4a).** Water's Fresnel term now samples the sky env map in
  the reflected direction (`sky_env_sample`, [`water.wgsl:327`](../rust/src/water/water.wgsl)),
  replacing the flat-`fog_color` stand-in — this is what fixed the pre-dawn "pink everywhere" wash.
- **Bindings shifted.** `group(0) @binding(9)` is now the `sky_sh` uniform. **Sky-vis takes `@binding(10)`.**

## 1. Motivation

Terrain, water and objects are lit by two terms:

- **Direct sun** — `sun_diffuse * N·L`, removed where a point is in shadow. Shadow composes (`max()`)
  the CSM cascades (near contact / objects) and the long-range **terrain sun-shadow mask**
  (`terrain_shadow.wgsl`), a heightfield sweep that marches *toward the sun*.
- **Ambient / sky fill** — now `sky_irradiance(n) * sun_ambient.w`: *directional* (varies with the
  surface normal) but still **positionally uniform**. A valley floor and an open hilltop with the
  same normal receive the **same** sky irradiance.

The remaining gap is *spatial*: the SH ambient knows which way a surface faces but not **how much sky
that point can actually see**. A point at the bottom of a gorge, in a ravine, or hard against a cliff
physically sees only a sliver of sky, yet gets the full hemisphere's irradiance. Ravines, coves and
mountain bases stay flat where contact darkening should ground them.

**Sky visibility** (sky-view factor — the ambient-occlusion analogue of the sun-shadow mask) supplies
the missing spatial factor: a per-column scalar `V ∈ [0,1]` = the cosine-weighted fraction of the sky
hemisphere *not* occluded by surrounding terrain. It **multiplies** the directional SH ambient, giving
a clean separable model — *directional from the normal* (SH) × *positional from the terrain* (`V`):

```
ambient = sky_irradiance(n) * sun_ambient.w * V(xz)
```

This is exactly the standard scalar-AO approximation the `sky_irradiance` comment anticipates; the
bent-normal upgrade (evaluate `sky_irradiance` from the *unoccluded* direction) is a natural follow-on
now that the ambient is directional (§7).

It must land on **all three surfaces at once** — terrain, water and objects — or a cove reads
inconsistently (dark ground beside bright water). The architecture makes this cheap: all three already
sample the terrain sun-shadow mask through the **shared `group(0)` frame bindings** and the same
world-xz→UV mapping. Sky visibility rides the exact same rails.

## 2. What it is (and what it is not)

- **Depends only on the heightmap.** Unlike the sun-shadow sweep, sky visibility is **sun-independent** —
  it is a property of the terrain shape alone. It is recomputed only when the heightmap changes
  (map load / terrain subdivision), *never* when the sun moves. This is the key cost lever.
- **Terrain-scale occlusion only.** It captures valleys, gorges, cliff bases, coves — occlusion by
  the heightfield. It does **not** capture buildings, foliage canopies or object-on-object occlusion
  (that is SSAO / the froxel's business, out of scope here). An object *inside* a building still reads
  the open-sky visibility of its ground column; acceptable and consistent with how the sun-shadow mask
  already treats objects.
- **A scalar per column, evaluated at the terrain surface.** Height dependence (a helicopter at 500 m
  over a gorge should see full sky) is a *refinement* (§7), not the first cut.

## 3. The sky-view factor (heightfield horizon scan)

For a column at world-xz **p** with terrain height `h0`, march the heightfield outward in **K**
azimuthal directions `φ_k`. Along each ray track the maximum *tangent of the horizon elevation
angle*:

```
t_k = max over the march of ( h(sample) - h0 ) / horizontal_distance     (clamped >= 0)
```

`t_k` is the steepest skyline the terrain raises in that azimuth. The cosine-weighted (Lambertian)
visible-sky fraction reduces to a clean closed form. The hemisphere integral with a per-azimuth
horizon angle `α_k = atan(t_k)` is

```
V = (1/π) ∫₀^{2π} ∫_{α(φ)}^{π/2} cosθ sinθ dθ dφ
  = (1/π) ∫₀^{2π} ½ cos²α(φ) dφ
```

which discretizes over K azimuths to

```
V ≈ (1/K) Σ_k cos²(α_k)   and   cos²(α_k) = 1 / (1 + t_k²)
```

so **`V = mean_k( 1 / (1 + t_k²) )`** — no `atan`, no `cos`, just the tracked slope. Flat unoccluded
terrain gives `t_k = 0 → V = 1`; a column ringed by walls of slope `t` gives `V = 1/(1+t²)`. This is
the standard heightfield sky-view factor, treating the receiver as horizontal (surface-normal
weighting is a refinement, §7).

### Where it runs: CPU, not a compute shader

Because sky visibility is (a) sun-independent, (b) computed once per map, and (c) disk-cached
(§4a), computing it on the **CPU** is strictly simpler than a GPU dispatch and cache-native — the
result is already in host memory, so serializing it needs no texture-readback plumbing. `set_heightmap`
(`terrain/mod.rs:742`) already hands the renderer the full `heights: &[f32]`, so the scan reads the same
array the GPU texture was uploaded from. Run it on a worker thread (rayon over output rows) so it is off
the load path; upload the finished `V` array into the texture the shaders sample.

- **Grid:** *coarser* than the heightmap — the opposite of the sun mask's 2× supersample. Sky-view is
  low-frequency (smooth), so a `/2` or `/4` grid bilinear-filters to a visually identical result; the
  sun mask needs finer only because shadow *boundaries* are sharp. One `R16Float` (or `R8Unorm`)
  texture, `V` in the single channel. Downsampling here is the primary cost lever.
- **Scan:** K azimuths (start K=8; 16 for quality) per output texel, tracking `t_k = max slope`.
  Use **distance-increasing strides** (step size grows with distance) rather than one heightfield
  texel per step: occlusion is dominated by near terrain and distant ridges subtend little solid
  angle, so effective sample count grows ~log with radius, not linearly. Bounded radius (~a few
  hundred m to ~1 km).
- **Cost (Nogova 2048² worst case):** a `/2` grid (~1 M texels) × K=8 × ~32 effective steps ≈ 5 GFLOP
  → ~1 s single-core, ~0.15 s across 8 cores (rayon). Full-res + long radius is the upper end
  (sub-second multicore). Paid **once, ever**, then read from disk — so K/radius can be generous.
- **Use the BASE heightmap; ignore fractal subdivision.** Subdivision (`setTerrainGrid`) adds
  *high-frequency* detail that barely moves a low-frequency sky-view factor, which is set by the
  large-scale valleys/mountains already present in the base map. So compute sky-vis from the base
  heightmap and do **not** recompute on subdivision (the sun mask still does). This keeps the cache
  key stable (no mid-session hash churn) and avoids a redundant second computation per load.
- **Gating:** a `skyvis_gen` bumped when the base heightmap changes (mirror `mask_gen`, but not on
  subdivision-only updates). Never on sun motion.

**Rejected alternative — GPU compute.** A `terrain_skyvis.wgsl` dispatch would be faster wall-clock, but
to *cache* it (the whole point) you must add an async copy-texture-to-buffer + map-readback to get bytes
to serialize — real plumbing for a once-per-map op whose CPU cost is already sub-second and off-thread.
CPU compute + disk cache is the coherent pair.

## 4. Storage & plumbing (reuse the existing rails)

The sun-shadow mask is already promoted into the shared `group(0)` frame bindings (4/5/6) with a
world-xz→UV mapping uniform (`TerrainShadowMap`, binding 6) that terrain, water and objects all use.
Sky visibility reuses **the same mapping uniform and the same filtering sampler** — it is defined on
the same world extent — and needs only **one new texture binding**.

- **`group(0) @binding(10)`** — `terrain_skyvis_mask: texture_2d<f32>`, `Float{filterable:true}`,
  sampled with the existing `terrain_shadow_samp` (binding 5) and the existing `terrain_shadow_map`
  mapping (binding 6). No new sampler, no new uniform. (Binding 9 is now the `sky_sh` SH uniform.)
- Layout + bind group: extend `CameraGroup` in `gfx3d/mod.rs` — the layout entry block now ends at
  binding 9 (`sky_sh`, ~`mod.rs:544`); add binding 10 after it. Thread the skyvis view through the
  **exact pattern `sky_sh_buf` uses**: it is passed as a parameter into the bind-builder (`mod.rs:626`,
  bound at `mod.rs:690`) and the second builder (`mod.rs:3064`). Add a `skyvis_view: &wgpu::TextureView`
  parameter alongside it. Rebuild the bind on `skyvis_gen` bump (alongside `mask_gen`). A 1×1 stand-in
  view before a heightmap loads, exactly like `create_shadow_mask(device, 1, 1)`.
- Terrain owns the texture + the CPU scan (sibling to the sun-shadow sweep in `terrain/mod.rs`),
  exposes `skyvis_view()` + `skyvis_gen()` (mirroring `shadow_gen()` used at `lib.rs:688`).
- The froxel pass (`sky/mod.rs`) samples the sun-shadow mask for volumetric occlusion; it does **not**
  need sky visibility. No change there.

## 4a. Disk cache (compute once, ever)

The scan is a pure function of the base heightfield + options, so cache the result to disk keyed by
content — after the first load of a given map, subsequent loads read the blob and skip the scan.

- **Key** = `hash(base height bytes) ⊕ hash(options: grid-res, K, radius) ⊕ ALGO_VERSION`. Hash the
  actual `heights: &[f32]` bytes (already in hand at `set_heightmap`), **not** the map name: a content
  hash is robust to modded/regenerated maps and — combined with the base-map policy in §3 — never goes
  stale. `ALGO_VERSION` is a constant bumped whenever the math or blob layout changes, so old caches
  self-invalidate. A fast non-crypto hash (xxhash/fnv over the height bytes) is plenty and cheap.
- **Blob** = a small header (magic, `ALGO_VERSION`, dims, format, key) + the raw `V` texels. Tiny: a
  `/2` grid of a 2048² map at `R16Float` is ~2 MB (`R8Unorm` ~1 MB). Validate the header before trust;
  on any mismatch (version/dims/hash) recompute and rewrite.
- **Location.** The renderer has **no cache-dir convention today** (grep: none). The Rust side must not
  guess a path — C++ owns the user/config/cache dir, so pass a cache-root string across the FFI once
  (e.g. `wgr_set_cache_dir`) and let Terrain place `skyvis/<key>.bin` under it. If the root is absent/
  unwritable, degrade to compute-every-load (correctness unaffected).
- **Layering.** The in-session `skyvis_gen` recompute-on-change still governs live updates; the disk
  cache is only a load-time short-circuit under it. On a cache hit, upload the blob straight into the
  texture and bump `skyvis_gen` (so the frame group rebinds) without running the scan.

**Alternative considered — pack into the sun-shadow mask's free `.a`.** The mask is `Rgba16Float`
storing `(ceiling, halfband, strength, 0)`; `.a` is unused. Tempting (zero new bindings), but the
horizon scan would then run inside the *sun-gated* sweep — recomputing a sun-independent quantity on
every sun move, and forced onto the sun mask's `scale`×-finer grid (wasteful for a low-frequency
field). A separate, heightmap-gated, coarser texture is the right call. **Recommendation: separate
texture.**

## 5. Consumption — modulate ambient on all three surfaces

New frame helper in `shaders/frame.wgsl`, beside `terrain_sun_shadow`:

```wgsl
// Cosine-weighted fraction of visible sky at a terrain column [0,1] (1 = open sky).
// Position-only (evaluated at the terrain surface); modulates the AMBIENT term,
// orthogonally to terrain_sun_shadow which modulates the DIRECT sun term.
fn terrain_sky_visibility(world_xz: vec2<f32>) -> f32 {
    if (terrain_shadow_map.enabled < 0.5) { return 1.0; }   // no data → full sky
    let uv = (world_xz - terrain_shadow_map.origin) * terrain_shadow_map.inv_span
             + terrain_shadow_map.half_texel;
    if (any(uv < vec2(0.0)) || any(uv > vec2(1.0))) { return 1.0; }
    return textureSampleLevel(terrain_skyvis_mask, terrain_shadow_samp, uv, 0.0).r;
}
```

Apply as a tunable partial multiply so occluded areas darken but never go black — ambient is the only
thing keeping shadowed geometry off pure black, so a raw multiply would over-darken. Strength + floor:

```wgsl
// skyvis in [0,1]; strength scales the effect, floor keeps minimum fill.
let ao = max(mix(1.0, terrain_sky_visibility(xz), sky_vis_strength), sky_vis_floor);   // strength 0 = off
ambient *= ao;
```

Touch points — the ambient term is the SH `sky_irradiance(n) * sun_ambient.w` on the sky-lit path
(the flat `sun_ambient` only remains on the legacy fallback):

- **Terrain** — `terrain.wgsl` `fs_terrain` ([`:302-306`](../rust/src/terrain/terrain.wgsl)): the
  `if (sky_lit) { sun_ambient = sky_irradiance(n) * frame.sun_ambient.w; }` block. Multiply that by
  `ao`. For parity, also scale the legacy `sun_ambient` in the `else` case. World-xz is `in.world_xz`
  (already absolute).
- **Objects** — `shaders/shading.wgsl` `shade()` ([`:83`](../rust/src/shaders/shading.wgsl)):
  `var ambient = sky_irradiance(nrm) * frame.sun_ambient.w;` → multiply by `ao`, sampled at
  `world_abs.xz` (already computed at `shading.wgsl:73` for the sun-shadow lookup). Applies *before*
  the translucent-canopy `ambient *= 0.2`. Also scale the legacy `m_sun_ambient` branch. One insertion,
  covers per-draw and GPU-driven paths (both funnel through `shade()`).
- **Water** — `water.wgsl` `fs_water` ([`:314`](../rust/src/water/water.wgsl)):
  `rgb = body * (sun_ambient + sun_diffuse * ndl * 0.15 * sun_vis)`. Water still uses the **flat**
  `sun_ambient` (it did *not* adopt SH), so scale that `sun_ambient` term by `ao` at `in.base_xz`, and
  the **foam** ambient (`foam_color`) likewise so shaded coves don't sprout bright foam.
  - **Do NOT apply `ao` to the water's env-map reflection** (`sky_env_sample(refl_dir)`, the Fresnel
    term). That is a *directional specular* reflection, not diffuse hemisphere fill — a scalar sky-view
    factor is the wrong tool. Its own occlusion (a cliff standing in the reflected direction) is
    Stage 4b's job (real terrain reflection); the code already approximates the sun-toward case via
    `ter_raw * toward_sun`. Since grazing water is reflection-dominated (high Fresnel), sky-vis on the
    diffuse ambient is a *subtle* effect on water — visible mainly looking down at near, shaded coves.
    Correctness/consistency, not a headline change.

Local point/spot lights (`lights_contrib`) are **not** modulated — they are not sky ambient.

`ao` is path-agnostic (it scales whichever ambient term is live), so no extra `sky_lit` branching is
needed beyond the branches that already exist.

## 6. Tuning (ImGui + WgrRenderParams)

Per the render-params consolidation convention (`render-params-consolidation-plan.md`), route
controls through `WgrRenderParams` + `wgr_set_render_params`, **not** new per-setting FFI:

- `sky_vis_strength` — `0` = off (ship default off until validated), `1` = full.
- `sky_vis_floor` — minimum ambient retained in fully-occluded columns.
- Compute-side (rebuild on change, cheap since amortized): `K` azimuths, march radius. These can stay
  constants initially and be promoted to params only if tuning demands it.
- A **debug view** (Culling/Lighting ImGui tab) that visualizes `V` as greyscale over the terrain, the
  same way the sun-shadow mask has a debug path — essential for validating the horizon scan.

## 7. Refinements (explicitly out of the first cut)

1. **Height fade for elevated objects.** A high object over a gorge should recover full sky. Store the
   terrain surface height alongside `V` (or reuse the sun mask's data) and lerp `V→1` over a band of
   `(world_y − terrain_h)`. Needs a second channel or texture; defer until aircraft/tall-object AO
   looks wrong.
2. **Bent-normal ambient (the natural upgrade, now that ambient is directional SH).** The scalar `V`
   attenuates `sky_irradiance(n)` uniformly. Better: store the **average unoccluded direction** (bent
   normal) from the horizon scan alongside `V`, and evaluate `sky_irradiance(bent_normal) * V` — this
   samples the SH irradiance from the direction the point can actually see, giving *colored* ambient
   occlusion (a gorge floor open only to the blue zenith reads cool; a slope open toward a warm lit
   ridge reads warm) for free from the existing SH. Needs the scan to also accumulate a direction
   vector and one more texture channel (e.g. an `Rgba16Float` skyvis texture: `xyz` = bent normal,
   `w` = `V`). This is the physically-motivated endpoint; scalar `V` is its ship-first approximation
   and composes straight into it (same consumers, same sample). **Note:** the old draft listed
   "directional sky ambient via `sky_radiance(dir)`" as future — that half **already landed** as the
   SH irradiance; only the *bent-normal* half remains.
3. **Water env-map reflection occlusion (Stage 4b).** Sky-vis deliberately does not touch the water's
   specular sky reflection (§5). Reflecting the actual terrain (a cliff in a cove) is Stage 4b in
   `water-rendering-plan.md` — tracked there, not here.

## 8. Work breakdown

1. CPU sky-view scan in `terrain/mod.rs` (K-azimuth, distance-strided march, closed-form `V`), rayon
   over output rows, from the base `heights: &[f32]`. Coarse grid (`/2`..`/4`). **[no deps]**
2. Disk cache (§4a): content hash of the base height bytes + options + `ALGO_VERSION`; blob
   read/validate/write; FFI cache-dir root (`wgr_set_cache_dir`, C++ side supplies the path).
   Cache-miss → scan + write; cache-hit → upload blob, bump `skyvis_gen`, skip scan.
3. `terrain/mod.rs`: skyvis texture (`R16Float`, coarse grid), upload path, `skyvis_gen` gating (base
   heightmap only — *not* subdivision updates), `skyvis_view()`/`skyvis_gen()` accessors.
4. `gfx3d/mod.rs` `CameraGroup`: add `group(0) @binding(10)` to layout + both bind builders (mirror the
   `sky_sh_buf` parameter threading at `mod.rs:626`/`3064`); rebuild-on-`skyvis_gen`. `lib.rs`: pass
   `skyvis_gen`/view through the frame-group update (beside `shadow_mask_gen` at `lib.rs:688`).
5. `shaders/frame.wgsl`: `terrain_skyvis_mask` at `@binding(10)` + `terrain_sky_visibility()` helper.
6. Consumers: multiply the SH ambient by `ao` in `terrain.wgsl` (`sky_irradiance` block) and
   `shading.wgsl` (`ambient` var); multiply the flat `sun_ambient` + `foam_color` in `water.wgsl`.
   Do **not** touch the water env-map reflection.
7. `WgrRenderParams`: `sky_vis_strength`, `sky_vis_floor`; ImGui Lighting/Culling controls + debug
   greyscale view. Ship **off** by default; validate, then pick a default.

## 9. Risks / notes

- **Coordinate parity.** The sun-shadow sweep hard-won the world↔heightfield-texel transforms (its
  header comments + the culling plan's coord-system notes). The CPU scan indexes the same `heights`
  array in heightfield-texel space; the coarse output grid maps back to heightfield texels by a fixed
  ratio. Shader sampling uses the *existing* `TerrainShadowMap` mapping unchanged (same world span, so
  a coarser texel count is transparent — UV maps to `[0,1]` regardless of resolution).
- **Radius vs cost.** A too-short radius under-darkens deep valleys ringed by distant-but-tall ridges;
  too long wastes samples on ridges that subtend little solid angle (distance-strided marching bounds
  this). Because it is computed once and disk-cached, err toward a generous radius — the cost is paid
  a single time per map, ever. Expose it for tuning (a change bumps `ALGO_VERSION`/the options hash → a
  fresh cache blob).
- **Cache staleness.** Content-hash keying + the base-heightmap policy (§3) mean a cache blob matches
  exactly one heightfield+options+version; any mismatch recomputes. Never key on map name. A missing/
  unwritable cache dir degrades to compute-every-load, never to wrong data.
- **Double-darkening.** Sky visibility multiplies *ambient*; sun-shadow removes *direct*. They are
  orthogonal and must not be conflated — a sunlit valley floor with low sky-view is bright-but-
  contact-shadowed (correct), not doubly dark. Verify the two terms stay separate in each consumer.
- **Off-map / no-heightmap → `V = 1`** (full sky), matching `terrain_sun_shadow`'s off-map → `0`
  (unshadowed) convention: absence of data must never darken.
```
