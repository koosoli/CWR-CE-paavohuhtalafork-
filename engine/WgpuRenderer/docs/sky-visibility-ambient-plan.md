# Sky-visibility ambient plan (heightfield sky-view factor → ambient occlusion)

**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust). **Status:** PLAN (2026-07-11).

## 1. Motivation

Terrain, water and objects today are lit by two terms:

- **Direct sun** — `sun_diffuse * N·L`, removed where a point is in shadow. Shadow comes from
  two sources composed by `max()`: the CSM cascades (near contact / objects) and the long-range
  **terrain sun-shadow mask** (`terrain_shadow.wgsl`), a heightfield sweep that marches *toward the
  sun* and stores a per-column "shadow ceiling".
- **Ambient / sky fill** — `sun_ambient`, added **uniformly everywhere**, normal- and
  position-independent. This is the term that keeps shadowed ground from going black.

The gap: ambient is flat. A point at the bottom of a narrow valley, in a gorge, or hard against a
cliff face physically sees only a sliver of sky, yet it receives the *same* ambient as an exposed
hilltop that sees the whole dome. This flattens ravines, coves and the bases of mountains — exactly
the places where contact darkening should ground the geometry.

**Sky visibility** (a.k.a. sky-view factor, the ambient-occlusion analogue of the sun-shadow mask)
fixes this: a per-column scalar in `[0,1]` = the cosine-weighted fraction of the sky hemisphere that
is *not* occluded by surrounding terrain. It modulates the **ambient** term, orthogonally to the
existing sun-shadow which modulates the **direct** term.

Crucially it must land on **all three surfaces at once** — terrain, water and objects — or it reads
as an inconsistency (dark ground meeting bright water in the same cove). The existing architecture
makes this cheap: all three already sample the terrain sun-shadow mask through the **shared
`group(0)` frame bindings** and the same world-xz→UV mapping. Sky visibility rides the exact same
rails.

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

- **`group(0) @binding(9)`** — `terrain_skyvis_mask: texture_2d<f32>`, `Float{filterable:true}`,
  sampled with the existing `terrain_shadow_samp` (binding 5) and the existing
  `terrain_shadow_map` mapping (binding 6). No new sampler, no new uniform.
- Layout + bind group: extend `CameraGroup` in `gfx3d/mod.rs` (the binding-4..8 block at
  `mod.rs:495`) with binding 9; thread the skyvis view through the same path that lends the
  shadow-mask view (rebuild on `skyvis_gen` bump alongside `mask_gen`). A 1×1 stand-in view before a
  heightmap loads, exactly like `create_shadow_mask(device, 1, 1)`.
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

Apply as a tunable partial multiply so occluded areas darken but never go black — the ambient is the
only thing keeping shadowed geometry off pure black, so a raw multiply would over-darken. Introduce a
strength/floor:

```wgsl
// skyvis in [0,1]; strength scales the effect, floor keeps minimum fill.
let ao = mix(1.0, terrain_sky_visibility(xz), sky_vis_strength);   // strength 0 = off
ambient *= max(ao, sky_vis_floor);
```

Touch points (each already samples `sun_ambient`):

- **Terrain** — `terrain.wgsl` `fs_terrain`: `sun_raw = sun_diffuse * cos_fi * (1-shadow) + sun_ambient`
  → scale the `sun_ambient` addend by `ao`. World-xz is `in.world_xz` (already absolute).
- **Objects** — `shaders/shading.wgsl` `shade()`: both the `sky_lit` and legacy branches add
  `frame.sun_ambient` / `m_sun_ambient` → scale by `ao`, sampled at `world_abs.xz` (already computed
  at `shading.wgsl:70` for the sun-shadow lookup). One line, covers per-draw and GPU-driven paths
  (both funnel through `shade()`).
- **Water** — `water.wgsl` `fs_water`: `rgb = body * (sun_ambient + sun_diffuse * ndl * 0.15 * sun_vis)`
  → scale the `sun_ambient` term by `ao` at `in.base_xz`. Also scale the **foam** ambient
  (`foam_color = sun_ambient + ...`) so shaded coves don't sprout bright foam. Leave the Fresnel
  horizon-tint (`fog_color` mix) alone — that is a sky *reflection* stand-in, not ambient fill; it is
  the sky the water sees, and a cove's water still reflects the bright sky band it faces.

Local point/spot lights (`lights_contrib`) are **not** modulated — they are not sky ambient.

The multiply is path-agnostic (legacy gamma ambient or physical sky-lit irradiance both just get
scaled), so no `sky_lit` branching is needed in the consumers.

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
2. **Surface-normal (bent-normal) weighting.** §3 treats the receiver as horizontal. Weighting the
   per-azimuth contribution by the surface normal (a steep slope facing a wall sees even less) sharpens
   cliff/slope AO. Cheap to add in the consumers if the normal is available (terrain and objects both
   have it); water is ~horizontal so it is moot there.
3. **Directional sky ambient via `sky_radiance(dir)`.** Once the procedural-sky module exposes an
   importable `sky_radiance(dir)` (already a prerequisite for water Stage 4a sky reflection,
   see `water-rendering-plan.md`), the ambient could integrate *actual* sky radiance over the visible
   hemisphere (per bent normal) instead of scaling a single `sun_ambient` scalar — colored ambient
   occlusion (bluer up, warmer near a lit ridge). This is the physically-correct endpoint; the scalar
   `V` multiply is its cheap, ship-first approximation and composes forward into it.

## 8. Work breakdown

1. CPU sky-view scan in `terrain/mod.rs` (K-azimuth, distance-strided march, closed-form `V`), rayon
   over output rows, from the base `heights: &[f32]`. Coarse grid (`/2`..`/4`). **[no deps]**
2. Disk cache (§4a): content hash of the base height bytes + options + `ALGO_VERSION`; blob
   read/validate/write; FFI cache-dir root (`wgr_set_cache_dir`, C++ side supplies the path).
   Cache-miss → scan + write; cache-hit → upload blob, bump `skyvis_gen`, skip scan.
3. `terrain/mod.rs`: skyvis texture (`R16Float`, coarse grid), upload path, `skyvis_gen` gating (base
   heightmap only — *not* subdivision updates), `skyvis_view()`/`skyvis_gen()` accessors.
4. `gfx3d/mod.rs` `CameraGroup`: add `group(0) @binding(9)` to layout + bind group; thread the view;
   rebuild-on-`skyvis_gen`. `lib.rs`: pass `skyvis_gen`/view through the frame-group update (beside
   `shadow_mask_gen` at `lib.rs:688`).
5. `shaders/frame.wgsl`: `terrain_skyvis_mask` binding + `terrain_sky_visibility()` helper.
6. Consumers: `terrain.wgsl`, `shaders/shading.wgsl`, `water.wgsl` ambient (+ water foam) multiply.
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
