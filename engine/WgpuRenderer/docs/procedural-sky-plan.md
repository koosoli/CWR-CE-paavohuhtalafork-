# Plan: Procedural atmospheric sky (analytic → LUT scattering, sun/moon/stars/clouds)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** IN PROGRESS (2026-07-07). Stages 0+1 landed and user-confirmed working (+ integration
fixes: horizon haze, legacy clouds/horizon suppressed — see §9). Stage 2 in progress. Sky-based
lighting + light-effects stages added (§3, §8).

> Follows the HDR pipeline ([hdr-pipeline-plan.md](hdr-pipeline-plan.md)). The legacy model-based
> skydomes are LDR-authored, heavily block-compressed (visibly polygonal), and clip/behave wrong under
> a linear-light HDR target. This plan replaces them with a fully procedural, physically-motivated sky
> driven by the game's existing celestial parameters, tunable live, correct in both SDR and HDR modes.

---

## 0. Where we are today (verified 2026-07-06)

- **Current sky = model skydomes.** `Landscape::DrawSky` ([LandscapeRender.cpp:1645](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1645))
  positions and draws `_skyObject` / `_starsObject` / `_sunObject` / `_moonObject` / `_cloudObj[]`,
  loaded in [Landscape.cpp:54-223](../../Poseidon/World/Terrain/Landscape.cpp#L54). They are flagged
  `NoZBuf | NoZWrite | ClampV | NoShadow | IsAlphaFog | FogDisabled` and routed to `PassId::Sky`
  purely by the `NoZBuf` bit ([RenderState.hpp:166](../../Poseidon/Graphics/Core/RenderState.hpp#L166)),
  emitted as `FramePassKind::Sky` first in the frame ([BuildFrame.hpp:20](../../Poseidon/Graphics/Rendering/Frame/BuildFrame.hpp#L20)).
- **The wgpu renderer has no sky concept.** Those draws arrive as ordinary no-Z 3D geometry; the only
  "background" the Rust side produces is the per-frame **clear color** (`frame.clear`, sRGB-decoded to
  linear when HDR is on, [lib.rs:413-429](../rust/src/lib.rs#L413)). The one atmospheric effect present
  is **linear distance fog** blending to `fog_color` (`frame.wgsl:105`, applied in terrain/3D shaders).
- **Celestial state is fully computed on the CPU** in `LightSun::Recalculate`
  ([Lights.cpp:100-276](../../Poseidon/Graphics/Rendering/Lighting/Lights.cpp#L100)), owned by
  `Scene::MainLight()`. It already produces: sun/moon direction, `_moonPhase`, `_nightEffect`,
  `_starsVisible`, `_sunColor`, `_skyColor`, `_sunSkyColor`, diffuse/ambient, star orientation.
  Overcast is separate world weather (`_actualOvercast`, [WorldSetup.cpp:1256](../../Poseidon/World/WorldSetup.cpp#L1256)).
- **Only three sun terms cross the FFI today** — `WgrCamera.sun_diffuse / sun_ambient / sun_dir_world`
  ([wgpu_renderer.hpp:315](../include/wgpu_renderer.hpp#L315) / [ffi.rs:268](../rust/src/ffi.rs#L268)),
  landed in group(0) UBO `Frame` ([frame.wgsl:27](../rust/src/shaders/frame.wgsl#L27)). **Moon dir,
  moon phase, night factor, sky color, star orientation, and overcast are NOT forwarded** — they only
  fed the legacy CPU/GL33 sky path. We will need to add them.
- **HDR pipeline gives us the target and the resolve.** Scene renders into an `Rgba16Float` HDR texture
  (`scene_view`) when `WGR_HDR != 0`, then a fullscreen **Hable tonemap** pass (`tonemap.rs` /
  `tonemap.wgsl`) applies exposure + grade + sRGB encode to the swapchain. On the SDR path `tonemap` is
  `None` and the scene draws **straight to the swapchain, gamma-naive**. Pipelines gate a `linear`
  shader-def constant on `surface_format == Rgba16Float`.
- **Reversed-Z.** Depth cleared to `0.0` (far), scene pipelines compare `GreaterEqual`
  (`DEPTH_FORMAT = Depth24PlusStencil8`). The only existing fullscreen pass is the tonemap
  (vertex-buffer-less, `draw(0..3)`, `depth_stencil: None`) — the natural template for a sky pass.

---

## 1. Design decisions (proposed)

1. **Fullscreen procedural sky, not geometry.** A single fullscreen-triangle pass reconstructs the
   world-space view ray per pixel and evaluates a sky radiance function. No dome mesh, no texture
   compression artifacts, resolution-independent.
2. **Sky model roadmap: start with a single-scattering raymarch (Nishita-style Rayleigh + Mie), then
   upgrade to a Hillaire-style LUT model** (transmittance + multiscatter + sky-view LUT + aerial
   perspective). Single-scatter is enough for a dramatic first light and to prove the plumbing; the LUT
   model buys realistic twilight, cheap runtime, and a transmittance source that sun/moon/clouds/aerial
   all reuse. (Analytic Preetham/Hosek-Wilkie rejected as the endpoint: weaker sunsets, no aerial
   perspective, awkward night.)
3. **Radiance-first, tone-mapped by the existing pipeline.** The sky writes **linear scene-referred
   radiance** into the HDR target and lets the Hable resolve handle exposure/encode — never
   pre-tonemap. On the **SDR path** (no resolve pass) the sky shader applies a local exposure + Hable +
   sRGB encode itself, gated on the same `linear` shader-def constant the other pipelines use. One
   shader, both modes.
4. **Sky drawn first, depth-test off.** Emitted at the start of the first scene segment right after the
   color clear, depth write **off**, depth test **off**. Geometry then overdraws it. This is robust
   against the multi-segment depth-clear structure and trivially correct; the fullscreen overdraw is
   cheap and we have GPU headroom. (Optimization deferred: draw sky *after* opaque with `GreaterEqual`
   depth test against far-plane z so covered pixels are rejected — see §6.)
5. **Driven by real game params, tunable by authored params.** Celestial inputs (sun/moon dir, phase,
   night factor, overcast) come live from `LightSun`. Look/tuning inputs (turbidity, Rayleigh/Mie
   coefficients, sun disc size/intensity, ground albedo, star/cloud params, per-ToD overrides) are
   authored, exposed through a new **Sky** ImGui tab, mirroring the Tonemap tab's plumbing exactly.
6. **Legacy skydomes hidden on wgpu only — GL33 keeps them.** The dome/sun/moon/stars/cloud meshes are
   currently drawn *by* the wgpu renderer (they arrive as generic no-Z geometry). They must be
   suppressed **backend-locally**: do NOT gate `Landscape::DrawSky` or the shared frame-graph
   classification — that code is backend-agnostic and would blank GL33's sky too. Instead drop the
   `FramePassKind::Sky` draws when the wgpu backend consumes/translates the built frame (in `EngineWgpu`
   as it walks frame ops → FFI), behind the procedural-sky flag. Keep a runtime A/B toggle so the old
   meshes can be re-enabled during development.
7. **GL33 untouched.** Entirely a wgpu-internal feature behind a flag; GL33 keeps the model skydomes.
8. **`WgrSky` is a dedicated uniform/storage buffer bound in the sky pass — not an extension of
   `WgrCamera`.** Clean separation of celestial+authored sky state from per-camera state, carries the
   authored look params, and avoids bloating the group(0) `Frame` UBO that every scene pipeline binds.
   Live celestial fields duplicated into it as needed (sun_dir can still be read from group(0) if the
   sky pass binds it, but the moon/star/overcast fields have no home in `WgrCamera` regardless).

---

## 2. Data the sky needs, and how it gets there

### 2.1 New FFI surface — `WgrSky` uniform + `wgr_set_sky`

Mirror the Tonemap plumbing (the fully-worked recent example). Add a `#[repr(C)]` `WgrSky` struct
declared identically in [wgpu_renderer.hpp](../include/wgpu_renderer.hpp) and
[ffi.rs](../rust/src/ffi.rs), with a matching `static_assert`/size assert, plus
`wgr_set_sky(WgrRenderer*, const WgrSky*)`. Two categories of fields:

**Live celestial (per-frame, from `LightSun` / world):**
- `sun_dir_world` (vec3) — reuse `MainLight()->Direction()` (already forwarded in `WgrCamera`; the sky
  pass can read group(0), so this may not need duplicating).
- `moon_dir_world` (vec3) — **new**, from `MainLight()->MoonDirection()`.
- `moon_phase` (f32) — **new**, `_moonPhase`.
- `night_factor` (f32) — **new**, `_nightEffect` (0 by day → 1 deep night); drives star/moon
  visibility and moonlight balance.
- `star_orientation` (mat3 or quat) — **new**, from `sun->StarsOrientation()`.
- `overcast` (f32) — **new**, world `_actualOvercast`; drives cloud coverage + desaturation.
- `sun_color` / `sun_sky_color` (vec3) — optional; the LUT model can derive these, but forwarding the
  engine's values keeps art direction consistent during transition.

**Authored look (changes only on tweak / per-ToD keyframe):**
- Atmosphere: `rayleigh_scattering` (vec3), `mie_scattering` (f32), `mie_g` (anisotropy),
  `turbidity`/`ozone` (vec3), `planet_radius`, `atmosphere_height`, `ground_albedo` (vec3).
- Sun disc: `sun_angular_radius`, `sun_disc_intensity`, limb-darkening coeffs.
- Moon disc: `moon_angular_radius`, `moon_brightness`, `earthshine`.
- Stars: `star_intensity`, `star_density`, `milkyway_intensity`.
- Clouds (later stages): `cloud_coverage`, `cloud_density`, `cloud_altitude`, `wind` (vec2),
  `cloud_color`/absorption.
- Global: `sky_exposure` / intensity scalar, `enabled` flag.

### 2.2 C++ side (`EngineWgpu`)

- Add `Engine::SkySettings` struct + `SupportsSky() / GetSkySettings() / SetSkySettings()` (and an
  auto/per-ToD toggle, like `GetTonemapAuto/SetTonemapAuto`) as virtuals on the base `Engine`
  ([Engine.hpp:946](../../Poseidon/Graphics/Core/Engine.hpp#L946) neighborhood), default no-op/false so
  GL33 disables the tab.
- Override in [EngineWgpu.hpp](../EngineWgpu.hpp)/[.cpp](../EngineWgpu.cpp): hold `_sky`, a `PushSky()`
  that translates `SkySettings` → `WgrSky` and calls `wgr_set_sky`, and per-frame packing of the live
  celestial fields from `GScene->MainLight()` alongside the existing `WgrCamera` fill
  ([EngineWgpu.cpp:718-799](../EngineWgpu.cpp#L718)).
- Optional per-ToD authored presets `kSkyPresets[]` + `UpdateAutoSky(hour)` interpolation, mirroring
  `kTonemapPresets` / `UpdateAutoTonemap` ([EngineWgpu.cpp:56-108, 1588](../EngineWgpu.cpp#L56)).

### 2.3 Rust side

- New module `engine/WgpuRenderer/rust/src/sky/` (`mod.rs` + `sky.wgsl`), plus `set_sky` on `Renderer`
  storing `sky_params: ffi::WgrSky` (mirror `tonemap_params`, [lib.rs:67,226](../rust/src/lib.rs#L67)).
- LUT precompute textures/passes live in this module (Stage 2+).

---

## 3. Rendering stages

Each stage is independently buildable and visually meaningful; the tree never sits broken.

### Stage 0 — Plumbing + flat gradient (scaffold)
- New `Sky` fullscreen pass modeled on `Tonemap::new`; render into `scene_view` at the start of the
  first scene segment, depth off (§1.4). Reconstruct the world view ray from inverse view-proj
  (group 0 already has `proj`/`view`).
- Add `WgrSky` FFI + `wgr_set_sky` + `SkySettings`/virtuals + `EngineWgpu` packing + `set_sky`.
- Shader outputs a trivial **two-color vertical gradient** (horizon→zenith) tinted by `sun_dir` — just
  enough to confirm HDR-linear output vs SDR local-tonemap output both look right and the params flow.
- Drop the `FramePassKind::Sky` draws in the wgpu frame-op translation (backend-local, §1.6); GL33
  keeps drawing them.
- **Exit:** procedural background visible in both `WGR_HDR=1` and SDR, reacts to time of day, no
  skydome meshes drawn on wgpu; GL33 unchanged.

### Stage 1 — Clear-sky single scattering + sun disc
- Replace the gradient with **Nishita-style single scattering**: raymarch the view ray through a
  spherical atmosphere, Rayleigh + Mie in-scattering toward the sun, optical-depth transmittance
  (small step counts, e.g. 16 view / 8 light — headroom is ample). Produces real sky-blue, reddening
  toward the horizon, and **dramatic Mie-forward sunrise/sunset** near the sun.
- **Sun disc** with limb darkening, sized by `sun_angular_radius`, attenuated by atmospheric
  transmittance so it reddens at low angles.
- **Exit:** believable clear-day sky and a sunrise/sunset that actually goes orange→red.

### Stage 2 — Hillaire LUT model (production quality)
Implemented in two increments to de-risk (Stage 1's single-scatter march already looks "epic" — don't
throw it away in one step, and there is no runtime visual test in CI):
- **2a (this stage):** add a **transmittance LUT** (2D, `transmittance(r, μ_sun)`) and an isotropic
  **multi-scattering LUT** (2D, `Ψ(r, μ_sun)`, Hillaire's closed-form infinite-scatter sum), both
  render-to-texture passes that depend **only on atmosphere params** (sun is a LUT axis), so they rebuild
  only when those params change (dirty-flagged), not per frame. **Keep the outer view march** but replace
  its inner light-ray loop with a transmittance-LUT fetch (faster + accurate) and add the multiscatter
  term from the LUT. This buys realistic **twilight / blue-hour** and a reusable transmittance LUT while
  preserving the Stage 1 look.
- **2b (later, perf):** add the per-frame **sky-view LUT** (small, e.g. 192×108, non-linear lat-long
  around the camera) and have the fullscreen pass sample it instead of marching. Pure optimization +
  smoothing; deferred because we have GPU headroom and the march-based look is already good.
- Transmittance LUT becomes the shared attenuation source for the sun disc, moon disc, **sun colour**
  (§8 sky-based lighting), and (later) aerial perspective and clouds.
- **Exit (2a):** convincing twilight/blue-hour, transmittance + multiscatter LUTs available downstream.

### Stage 3 — Night sky: moon + stars
- **Moon disc** from `moon_dir_world`: lit hemisphere by `moon_phase`, angular size, soft halo, subtle
  earthshine; attenuated by transmittance like the sun.
- **Stars**: procedural hash-based starfield sampled by world direction, rotated by `star_orientation`,
  faded by `night_factor` and washed out near the moon/horizon glow. Optional Milky-Way band via a
  baked equirect texture (small, HDR) for extra drama. `_starsVisible` / `night_factor` gate visibility.
- Balance moonlight vs the sun-light path already fading via `NightEffect` (point lights premultiply by
  it today).
- **Exit:** clear nights show stars + a phased moon; overcast hides them.

### Stage 4 — Aerial perspective (unify with fog)
- Replace/augment the existing linear distance fog with **atmospheric aerial perspective**: apply
  view-ray transmittance + in-scatter (from the LUTs, or a small froxel volume à la Hillaire) to
  terrain and objects so distant geometry dissolves into the *same* sky it sits against. Keep the old
  `fog_*` uniforms as an artistic override/floor.
- **Exit:** no seam between distant terrain and sky; haze responds to sun angle and turbidity.

### Stage 5 — Clouds
- **5a — 2D layer:** animated domain-warped fBm cloud layer on the sky hemisphere, coverage from
  `overcast`, lit by sun/moon transmittance, wind-scrolled. Big visual payoff, cheap.
- **5b — Volumetric (optional, later):** raymarched clouds (Worley+Perlin density, Henyey-Greenstein
  scattering, cheap light-march + ambient), temporal reprojection to keep sample counts low. Gated,
  since it's the one genuinely expensive piece.
- **Exit:** dynamic cloud coverage tied to weather; overcast skies read as overcast.

### Stage 6 — Sky-based lighting (irradiance ambient + atmosphere-sourced sun colour)
Source the scene's environmental lighting from the same atmosphere, without going to full PBR. Sequenced
**after Stage 2** (the LUTs make it cheap and exact) but the hemisphere tier can land earlier. See §8 for
the reasoning and the "why not per-pixel / why not a cubemap for diffuse" discussion.
- **Ambient (biggest win):** replace the flat constant ambient with sky irradiance. Compute a compact
  per-frame representation — **L1/L2 spherical-harmonics** (9 RGB coeffs) or a 3-colour hemisphere
  (zenith/horizon/ground) — **once per frame**, not per pixel, and evaluate it per fragment (a few dot
  products). Decouples cost from pixel count/overdraw entirely. **Decision: prototype via a CPU port of
  the scatter function → SH first** (fastest to stand up), even though it duplicates the atmosphere math;
  move the integration to a GPU pass reading the sky-view LUT once 2b lands (single source of truth).
- **Sun colour/intensity from transmittance:** `sun_colour = illuminance_TOA · T(altitude, μ_sun)` from
  the Stage 2 transmittance LUT — one fetch per frame. Reddens the directional light in lockstep with the
  sky at sunset. Feeds the point where we already read `MainLight()->Diffuse()` (PushSky / DrawSectionTL).
- **Sky specular reflections (later):** a small per-frame **sky cubemap** (with roughness mips) sampled
  in the reflection vector for non-planar shiny surfaces (cockpit glass, etc.). Complements — does not
  replace — **planar reflections for water** (a separate later effort that reflects scene + sky with
  parallax). The sky cubemap is the point at which rendering the sky to a texture becomes worthwhile;
  diffuse ambient does **not** need it (SH suffices).
- **Exit:** ambient + sun colour + (later) reflections all driven by the atmosphere, coherent across ToD.

### Stage 7 — Light effects / post (bloom + lens flares)
Pairs with the HDR pipeline (the sun disc already writes radiance ≫ 1.0).
- **Bloom:** threshold/prefilter → progressive downsample → upsample-combine, composited before/around
  the tonemap resolve. Makes the bright sun/horizon bloom naturally.
- **Lens flare / sun glare:** screen-space, keyed off the sun's projected position + an occlusion test
  (depth sample or the existing sun-shadow), ghosts/halo along the sun→centre axis. Optional dirt mask.
- **Exit:** epic sunrises get the glow + flare that sells them.

---

## 4. SDR / HDR correctness (the load-bearing detail)

- **HDR path:** sky pipeline built with `format = surface_format (Rgba16Float)`, `linear` shader-def
  `= 1`. Sky writes **un-encoded linear radiance**; the Hable resolve applies `sky_exposure`/global
  exposure, tonemap, sRGB encode. Writing clamped/sRGB here would double-encode.
- **SDR path:** `tonemap` is `None`, so nothing resolves. Sky pipeline targets `config.format`,
  `linear = 0`; the shader applies its own exposure + Hable + `linear_to_srgb` (reuse the curve from
  `tonemap.wgsl`) so the LDR background is tone-mapped like the HDR one instead of hard-clipping the
  bright sun/horizon. Same source, branch on the `linear` const.
- Radiance scale: pick a physical-ish unit for sun/sky radiance and let exposure map it — keep the sun
  disc well above 1.0 so bloom/auto-exposure (HDR plan) have something to work with.

## 5. ImGui tuning (Sky tab)

Add `DrawSkyTab()` next to `DrawTonemapTab` ([DebugOverlay.cpp:1523](../../Poseidon/Dev/Debug/DebugOverlay.cpp#L1523))
and register it in `DrawMainWindow` ([~:1773](../../Poseidon/Dev/Debug/DebugOverlay.cpp#L1773)). Guard
on `SupportsSky()`; read `GetSkySettings()`, drive sliders with a `changed` accumulator, write back with
`SetSkySettings()` (immediate, no `Defer` — a renderer-param setter, like tonemap). Include an
**"Auto (time-of-day)"** checkbox + `BeginDisabled` idiom and a **copy-preset-to-clipboard** text box so
tuned values can be pasted into `kSkyPresets[]` — same affordances the Tonemap tab already has. Sliders:
turbidity, Rayleigh/Mie/ozone, mie_g, sun disc size/intensity, ground albedo, star/cloud params,
sky exposure, plus a "draw legacy skydome" A/B toggle.

## 6. Open questions / risks

- **Sky-pass placement vs perf:** Stage 0 draws sky-first (overdraw behind all geometry). If profiling
  shows the raymarch/LUT sampling is costly, switch to sky-*after*-opaque with reversed-Z
  `GreaterEqual` depth test against the far plane so only unwritten (background) pixels shade. The
  multi-segment depth-clear structure ([lib.rs:406-512](../rust/src/lib.rs#L406)) means the reliable
  depth to test against is the final segment's — validate before relying on it.
- **Moon/star params not yet in FFI:** confirm `StarsOrientation()` and `MoonDirection()` accessors and
  units; decide quat vs mat3 for star orientation.
- **Consistency with lighting:** the sky now defines "sky color"/ambient; eventually the object/terrain
  ambient term should sample the sky (LUT or a tiny irradiance SH) instead of the CPU `_ambient`, so
  shading and background agree. Out of scope here, noted for the Forward+/lighting work.
- **Overcast coupling:** overcast must both raise cloud coverage *and* flatten/desaturate the clear-sky
  scattering, or clear-sky blue will show through thin clouds.
- **Sky model endpoint:** Nishita→Hillaire is the recommended path; if Stage 1 already looks good
  enough for the game's needs, Stage 2 can be deferred without blocking stars/clouds.

---

## 7. Suggested landing order

Stage 0 (plumbing + gradient) → Stage 1 (single-scatter + sun) — these two give the "minimal clear sky
with a procedural sun" the brief asks for [DONE]. Then Stage 2 (LUT: 2a transmittance+multiscatter now,
2b sky-view later) and Stage 3 (moon + stars) for the full day-night cycle, Stage 6 (sky-based lighting)
once the LUTs exist, Stage 4 (aerial perspective) to unify with terrain, Stage 7 (bloom + lens flares)
for polish, and Stage 5 (clouds) with 5b volumetric strictly optional. Each stage is one PR behind the
procedural-sky flag.

---

## 8. Sky-based lighting — why not per-pixel, why not a cubemap (for diffuse)

Diffuse ambient irradiance is **extremely low-frequency and spatially invariant** over the scene (the
sky over one tree equals the sky over the next; it depends only on the surface normal and the ~uniform
sun direction). So:
- **Do not** recompute the scattering integral per shaded pixel — it repeats near-identical work
  millions of times, and without a depth pre-pass overdraw multiplies the waste.
- **Do** precompute a compact representation **once per frame** (SH L1/L2, or a 3-colour hemisphere) and
  evaluate it per fragment in a few ALU ops. The expensive sky evaluation then runs a *fixed small number
  of times per frame*, decoupled from pixel count — so overdraw of the cheap ambient term is a non-issue
  and no depth pre-pass is needed on its account.
- **No cubemap needed for diffuse.** A cubemap only pays off for **specular** reflections (roughness
  mips); for irradiance, SH is strictly better/cheaper. Build order: CPU-port → SH now; GPU integration
  of the sky-view LUT once 2b lands.

**Sun colour from transmittance:** direct sunlight at the ground = TOA solar spectrum × transmittance
along the path to the sun, `T = exp(-∫ (β_R·ρ_R + β_M,ext·ρ_M) ds)`. Wavelength-dependent Rayleigh
extinction reddens the beam as the sun lowers (long path) — the same mechanism that reddens the sky, so
sun and sky redden together at the same angle. In the LUT model this is one `transmittance_lut(altitude,
μ_sun)` fetch per frame. The geometric dimming on surfaces is already `N·L`; transmittance supplies only
the beam colour/attenuation. This replaces the engine's ad-hoc `sinSun` sunset curve
(`LightSun::Recalculate`) at the point we already read `MainLight()->Diffuse()`.

---

## 9. Follow-ups from first light (2026-07-07)

Observed once Stages 0/1 were running (user feedback), to fix:
1. **Horizon haze lights up the night sky.** The haze blends the near-black night sky toward `FogColor()`
   (which stays mid-grey at night). `night_factor` is already in the sky uniform (`ground_albedo.w`) —
   quick fix: fade haze by `(1 - night_factor)`; better: scale the haze colour by the sky's own horizon
   radiance; real fix: Stage 4 aerial perspective (naturally dark at night). Interim `(1-night)` fade now.
2. **"Grey stripe across the sky" with haze off.** Same sky↔fogged-terrain seam the haze masks; possibly
   compounded by the single-scatter march's ground-intersection handling. Principled fix = Stage 4.
3. **Sun position stutters (discrete steps, worst when flying).** `MainLight()->SunDirection()` only
   refreshes on `LightSun::Recalculate`'s coarse clock-advance cadence, no interpolation. Fix in `PushSky`
   (wgpu-local): slerp the pushed sun/moon direction toward the latest each frame (cheap), or recompute
   the direction analytically per frame. Independent of the shader.
4. **Bloom + lens flares** wanted — captured as Stage 7.
