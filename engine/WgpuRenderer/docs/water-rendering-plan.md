# Plan: Procedural water rendering (waves, transparency, refraction, planar reflection)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** PLANNED (2026-07-08)
**Roadmap slot:** Phase 3 (Stages 1–3, 4a, 5) + Phase 5 (Stage 4b); Stage 1 waves may pull forward to
Phase 1. Depends on the **depth prepass** (Phase 2), *not* full Forward+. See
[implementation-roadmap.md](implementation-roadmap.md).

> The **look** half of the water rework. Builds on the flat GPU CDLOD water surface from
> [water-cdlod-geometry-plan.md](water-cdlod-geometry-plan.md) (its prerequisite) and turns it into
> parametric, geometrically-waved water with depth-based colour, transparency, screen-space refraction,
> and planar reflections — coupled to the procedural sky ([procedural-sky-plan.md](procedural-sky-plan.md))
> and tonemapped by the HDR pipeline ([hdr-pipeline-plan.md](hdr-pipeline-plan.md)). Replaces the ugly
> animated normal-map texture entirely.
>
> **Same prime directive: zero gameplay impact.** Waves are cosmetic vertex displacement only; buoyancy
> and all sea-level reads stay on the flat CPU plane in `Landscape` (see the geometry plan §5). Waves are
> kept gentle so floating objects never visibly detach from the surface.

---

## 0. Starting point (what the geometry plan hands us)

- A flat water plane at `y = sea_level`, drawn as an instanced CDLOD grid (shared 32×32 unit mesh +
  skirts + distance morph, copied from terrain) via `WGR_CMD_DRAW_WATER`, module
  `engine/WgpuRenderer/rust/src/water/` (`mod.rs` + `water.wgsl`).
- Rendered **after** opaque terrain+objects, into the HDR `scene_view` (`Rgba16Float`), depth-tested
  reversed-Z `GreaterEqual`, depth-write off, `Alpha` blend-ready, binding the shared frame group(0).
- Shorelines are cut by the depth test against the already-drawn seabed/coast; the terrain heightmap is
  already uploaded to the renderer and reachable.
- Reusable via `#import`: `frame` (`frame.proj/view/cam_pos`, sun dir/diffuse/ambient, froxel `apply_fog`,
  `fog_factor`, `reverse_z`, `terrain_sun_shadow`, CSM `shadow_map`, `lights`), `color::srgb_to_linear`,
  `lighting::{sun_light,lights_contrib}`, `shadow::shadow_strength`
  ([frame.wgsl](../rust/src/shaders/frame.wgsl), [gfx3d/shader3d.wgsl:194](../rust/src/gfx3d/shader3d.wgsl#L194)
  is the reference consumer).

### Two capabilities the renderer does NOT have yet (this plan adds them)

1. **Scene depth as a texture.** `ensure_depth` allocates the depth target with `RENDER_ATTACHMENT` only
   ([gfx3d/mod.rs:1406](../rust/src/gfx3d/mod.rs#L1406)); there is no depth-as-texture bind, no linear
   depth copy. Depth-based water colour, soft shorelines, and refraction-clamping all need it.
   **This is exactly what the depth prepass provides** ([depth-prepass-plan.md](depth-prepass-plan.md),
   Phase 2): a sampleable **opaque-only** depth buffer — and opaque-only is precisely the seabed
   depth water wants (`water_depth = surface_y − seabed_y`). Under MSAA the prepass exposes a
   `min`-resolved single-sample depth (that plan §5), which water samples unchanged. **Consume the prepass depth if it has
   landed; otherwise self-provide** a swappable equivalent (add `TEXTURE_BINDING` + a depth-aspect view
   here). Write water's depth access as "sample an opaque-depth texture" so the source is
   interchangeable and Phase 3 never hard-blocks on Phase 2. Note: water needs only the **prepass**, not
   the Forward+ clustered light-culling — that is a parallel track that merely *enhances* water later
   (efficient many-light shading on the surface).
2. **Scene colour as a texture.** Nothing exposes `scene_view` for sampling. Screen-space refraction
   needs a readable copy of the opaque scene composited *before* water.
3. **Sky radiance in another shader.** `sky/sky.wgsl` has no `#define_import_path` and its LUTs live in a
   private group; there is no sky cubemap/probe ([procedural-sky-plan.md](procedural-sky-plan.md) §6/8
   noted planar reflections as a separate later effort). Reflections need either an importable sky
   evaluator or a planar re-render that includes the sky pass.

---

## 1. Design decisions

1. **Gerstner-sum waves in the vertex shader, analytic normals — FFT deferred.** Sum 4–8 Gerstner waves
   (per-wave: direction, amplitude, wavelength, steepness, phase speed) for horizontal+vertical
   displacement, plus their analytic partial derivatives for the surface normal/tangent. Cheap, no
   compute pass, deterministic, and the CDLOD grid we inherited already tessellates finely near the
   camera and morphs crack-free. A Tessendorf **FFT ocean** (compute-generated displacement + normal
   maps) is a *later, optional* upgrade for open-ocean realism; the game's seas are mostly calm coastal
   water where Gerstner is plenty. Low-frequency shape in the VS; **high-frequency normal detail in the
   FS** (a few extra procedural gradient terms / domain-warped value noise — fully procedural, no
   scrolling texture) so lighting stays crisp independent of tessellation, exactly like terrain's
   per-fragment normal ([terrain.wgsl:77-83](../rust/src/terrain/terrain.wgsl#L77)).

2. **Keep waves gentle and cosmetic.** Amplitudes on the order of the legacy `maxWave` (~0.1–0.3 m) so
   the visual crests never separate a boat hull from the flat buoyancy plane. Waves derive from a frame
   time uniform; they average to `sea_level`. Never read back into simulation.

3. **Depth-based colour + Beer-Lambert clarity.** Reconstruct the seabed world position from sampled
   scene depth and the water surface position; `water_depth = surface_y - seabed_y`. Blend
   `shallow_colour → deep_colour` and attenuate transmitted light by `exp(-extinction · water_depth)`
   (per-channel extinction = the "clarity"/turbidity control). This is what makes shallows read as
   turquoise and depths as dark blue, and gives a soft shoreline fade (small depth → near-transparent)
   without meshing the coast.

4. **Fresnel mix of reflection and refraction.** Schlick Fresnel on `dot(view, normal)` (F0 ≈ 0.02):
   near-grazing → mostly reflection (sky/scene), top-down → mostly refraction (depth-tinted scene
   beneath). Sun **specular glint** (sharp, HDR, blooms via the existing bloom pass) on the wave normals
   replaces the legacy specular texture.

5. **Screen-space refraction from a pre-water scene-colour copy.** Blit `scene_view` (opaque scene) into
   a sampleable `refraction_src` right before the water pass; water samples it at the screen UV perturbed
   by the water normal (scaled by `1/depth` so distortion shrinks with distance). Clamp the offset so it
   never samples a pixel *nearer* than the water (guard with the depth texture) to avoid bleeding
   foreground objects into the water. Optional slight chromatic split later.

6. **Planar reflection for a flat plane is exact — do it, but stage it.** Because water is one plane at a
   known height, a mirrored-camera re-render is geometrically exact and far better than SSR at the
   grazing angles that dominate a water surface. **Stage it to control cost:**
   - **6a — sky-only reflection first.** Reflect the view ray about the (perturbed) water normal and
     evaluate sky radiance along it. Cheapest, no extra scene pass, already covers most of what the eye
     reads on open water. Requires refactoring `sky/sky.wgsl` into an importable module (`#define_import_path
     sky`) exposing a `sky_radiance(dir)` the water FS can call — a clean win also usable by future
     specular/ambient work.
   - **6b — full planar scene reflection.** Add a reflection render target and a mirrored camera
     (reflect eye + frustum across `y = sea_level`, flip winding, user clip-plane at the waterline so
     under-water geometry isn't reflected). Re-render sky + terrain + major objects into it at **half
     resolution**, then sample by screen-space projection with normal distortion. Composite over 6a's
     sky reflection for rays that miss geometry.
     **Sequence this as Phase 5, after GPU-driven culling (Phase 4).** In the current CPU-driven model a
     reflection re-walks + re-submits the whole scene, doubling the submission cost that is already the
     documented bottleneck ([rendering-performance-plan.md](rendering-performance-plan.md)). Through the
     Phase-4 multi-view cull path ([gpu-object-rendering-plan.md](gpu-object-rendering-plan.md) Stage 3,
     [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)) a reflection is ≈ one extra cull
     dispatch + one indirect draw over the retained buffers (zero re-upload): the mirrored camera is
     "just another view." Add the water surface as an **instance-granularity clip plane** in that view's
     cull pass so below-water instances are rejected before draw. **Caveat:** GPU-driven removes the
     *submission* half, not the *raster* half — the reflected view still rasterizes/shades a scene;
     mitigate with half-res, reduced draw distance, opaque-only, on-screen-only gating. Terrain/water
     reflection is cheap regardless (CDLOD instance arrays re-selected for the mirrored camera); the
     retained-object model is what makes *object* reflections affordable.

7. **Sky-coupled per-map look.** A new `WgrWaterLook` uniform carries authored per-map fields: deep &
   shallow colour, extinction/clarity (vec3), Fresnel F0, sun-specular power/intensity, wave set
   (dirs/amp/wavelength/steepness/speed), wind, foam params. **Couple to the sky:** tint the deep/ambient
   term and the reflection by the procedural sky's ambient/time-of-day so night water goes dark and
   sunset water reddens — reuse `frame.sun_ambient`/`frame.sun_diffuse` (already atmosphere-derived when
   sky-lit, `sun_diffuse.w > 0.5`) and the froxel horizon tint, rather than a fixed colour. Exposed
   through a new **Water** ImGui tab mirroring the Tonemap/Sky tabs
   ([DebugOverlay.cpp:1523](../../Poseidon/Dev/Debug/DebugOverlay.cpp#L1523)) with per-ToD/per-map presets
   and copy-to-clipboard, plumbed via `Engine::WaterSettings` virtuals + `EngineWgpu` like `SkySettings`.

8. **HDR/linear + fog correctness, reuse the conventions.** Water pipeline built with `format =
   surface_format`, the `linear` override = `1` on `Rgba16Float`
   ([gfx3d/mod.rs:1226](../rust/src/gfx3d/mod.rs#L1226)); write un-clamped linear radiance (sun glint
   above 1.0 so it blooms), let the tonemap resolve. Apply the froxel `apply_fog` so distant water melts
   into the horizon/sky like the terrain does.

9. **wgpu-only, flagged, GL33 untouched.** Everything is behind the water path from the geometry plan;
   GL33 keeps its legacy water. Keep sub-toggles (waves / reflection / refraction) for bring-up and perf
   A/B.

---

## 2. Data + FFI additions

- **`WgrWaterLook`** uniform (declare in [wgpu_renderer.hpp](../include/wgpu_renderer.hpp) +
  [ffi.rs](../rust/src/ffi.rs) with a size `static_assert`, like `WgrSky`
  [ffi.rs:274](../rust/src/ffi.rs#L274)): deep_colour+_pad (vec4), shallow_colour (vec4), extinction+clarity
  (vec4), fresnel_f0/specular_power/specular_intensity/normal_strength (vec4), wind (vec2)+time-scale,
  foam params (vec4), an array of wave descriptors (vec4 each: `dir.xy, amplitude, wavelength` + a second
  vec4 `steepness, speed, phase, _`), and an `enabled`/control vec4. Set via `wgr_water_set_look(renderer,
  const WgrWaterLook*)`.
- **Per-frame time** for wave animation: reuse or extend an existing per-frame scalar (the sky already
  forwards celestial time; add a plain `time` float to `WgrCamera`/`WgrFrame` if not present) so the VS
  advances waves. Interpolate/slerp on the C++ side if the game clock is coarse (same stutter fix the sky
  plan §9.3 notes for the sun).
- **Scene depth + colour bindings (Rust):** add `TEXTURE_BINDING` to `ensure_depth`
  ([gfx3d/mod.rs:1406](../rust/src/gfx3d/mod.rs#L1406)) and expose a depth-aspect view; add a
  `refraction_src` HDR texture + a pre-water blit of `scene_view`. Bind both (plus the water look UBO and,
  for 6b, the reflection texture) as the water pipeline's group(1). None of this touches the FFI — it's
  internal to the renderer.
- **C++ `Engine::WaterSettings`** struct + `SupportsWater()/GetWaterSettings()/SetWaterSettings()` +
  auto/per-ToD toggle, default no-op on GL33 — mirror `SkySettings`
  ([Engine.hpp:946](../../Poseidon/Graphics/Core/Engine.hpp#L946) neighbourhood, procedural-sky-plan §2.2).
  `EngineWgpu` holds `_water_look`, a `PushWaterLook()` translating settings → `WgrWaterLook` →
  `wgr_water_set_look`, and optional `kWaterPresets[]` + `UpdateAutoWater(hour)`.

---

## 3. Rendering stages

Each stage is independently shippable and visually meaningful.

### Stage 1 — Gerstner waves + procedural normal + sun specular (no new render targets)
- VS: sum N Gerstner waves for xyz displacement over the flat plane; pass world pos + analytic
  tangent/bitangent. FS: build the normal from the analytic derivatives + high-frequency procedural
  detail; shade with `lighting::sun_light` + a sharp HDR sun specular; base colour = a constant
  deep/shallow guess; `apply_fog`. Waves/params hard-coded first, then from `WgrWaterLook`.
- **Exit:** the ocean visibly undulates with animated geometric waves and a bright sun glint; the ugly
  animated-normal-map texture is gone. Still opaque-ish, no reflection/refraction.

### Stage 2 — Depth-based colour + transparency + soft shoreline (adds depth texture)
- Make the scene depth sampleable; reconstruct seabed depth; `water_depth` → shallow/deep blend +
  Beer-Lambert clarity; alpha from a shallow fade so shorelines dissolve; enable `Alpha` blend.
- **Exit:** shallows turn turquoise, depths dark, and the waterline is a soft transparent fade over the
  visible seabed — no hard mesh edge. Clarity is tunable.

### Stage 3 — Screen-space refraction (adds scene-colour copy)
- Pre-water blit `scene_view` → `refraction_src`; sample it at a normal-perturbed, depth-scaled,
  depth-clamped screen UV; the deep-colour tint modulates the refracted sample by `water_depth`.
- **Exit:** the seabed/objects beneath the surface wobble with the waves; refraction reads correctly and
  doesn't bleed foreground.

### Stage 4 — Reflection (4a sky-only, 4b planar scene)
- **4a:** refactor `sky.wgsl` into an importable `sky` module; reflect the view ray about the water
  normal; Fresnel-mix sky reflection with the Stage 3 refraction.
- **4b:** mirrored-camera half-res planar re-render (sky+terrain+major objects, waterline clip plane);
  screen-space sample with normal distortion; composite over 4a where geometry is missed.
- **Exit:** water reflects the procedural sky and the coastline; Fresnel makes grazing water mirror-like
  and top-down water transmit. This satisfies the brief's "transparency, refraction, planar reflections".

### Stage 5 — Per-map look settings + Water ImGui tab + sky coupling
- `WgrWaterLook` fully wired; `WaterSettings` virtuals; `DrawWaterTab()`; per-map/per-ToD presets; tie
  deep/ambient/reflection tint to `frame.sun_*`/froxel so colour tracks time of day and weather.
- **Exit:** per-map water colour + clarity are authorable and live-tunable; night/sunset water looks
  right; consistent with the sky.

### Stage 6 — Foam (optional polish)
- Shoreline foam where `water_depth` is small; wave-crest foam from Gerstner steepness/Jacobian; subtle
  animated foam texture or procedural noise.
- **Exit:** breaking-edge and crest foam sell the surface.

### Stage 7 — Underwater view (optional, later)
- When the camera is below `sea_level`: underwater fog/tint + a from-below surface shade + caustics.
  Larger scope; captured here as a follow-up.

---

## 4. Cost & correctness risks

- **Planar reflection is the expensive item.** A second scene pass doubles some draw cost. Half-res,
  reduced set, on-screen-only gating, and defaulting to 4a (sky-only) on lower tiers keep it affordable.
  Reversed-Z + a user clip plane in the reflected pass, and the reflected pass has no valid froxel
  (view-dependent) — either build a reflected froxel or skip aerial fog in the reflection (acceptable).
- **Depth/colour as textures interact with the multi-segment structure.** The blit and the depth-aspect
  view must target the correct segment's resources ([lib.rs:707-810](../rust/src/lib.rs#L707)); the water
  pass reads the composited opaque scene, so it must run after all opaque ops in its segment (guaranteed
  by the geometry plan's op ordering).
- **Sky module refactor.** Making `sky.wgsl` importable must not regress the fullscreen sky pass; keep the
  entry `fs_sky` and add a pure `sky_radiance(dir)` the pass and the water share.
- **Wave/gameplay divergence.** Keep amplitudes small (decision 2). If a mission ever wants rough seas,
  that is a look-only knob — buoyancy stays flat. Document this so nobody wires waves into physics.
- **Boats & wakes.** Legacy ship wake spray reads `GetSeaLevel() - 0.1` ([Ship.cpp:825]); it stays on the
  flat plane. Visual wake/foam interaction with Gerstner waves is out of scope (foam stage is generic).

---

## 5. Landing order

Prerequisite: [water-cdlod-geometry-plan.md](water-cdlod-geometry-plan.md) complete (flat GPU ocean).
Then Stage 1 (waves+specular) → Stage 2 (depth colour+transparency) → Stage 3 (refraction) →
Stage 4a (sky reflection) → Stage 4b (planar reflection) → Stage 5 (per-map look + tab + sky coupling),
with Stage 6 (foam) and Stage 7 (underwater) as optional polish. One PR per stage, behind the water flag
with per-effect sub-toggles.
