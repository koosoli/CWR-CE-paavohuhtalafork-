# Plan: Consolidate the imgui-tweakable render params into one `WgrRenderParams` struct

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** PLANNED — revised 2026-07-12 (supersedes the 2026-07-08 draft). Concrete.
**Scope:** FFI ergonomics only — no shader, UBO, or visual change.

> **RND-030 reconciliation (2026-08-02):** the status line above is out of date: the consolidated parameter block is **implemented** as `WgrRenderParams` (`ffi.rs:589`), with its 368-byte layout locked by a compile-time ABI assert (`ffi.rs:1026`).
>
> The status line is left as written rather than rewritten, so the document's own history stays readable. See [RND-030-renderer-plan-reconciliation-20260802.md](../../../docs/roadmap/decisions/RND-030-renderer-plan-reconciliation-20260802.md).

> **Governing rule:** every ImGui-tweakable render parameter that crosses the FFI as a *setter* flows
> through a single monolithic `WgrRenderParams` struct (a struct of structs) pushed via one
> `wgr_set_render_params` call. Per-frame **runtime** state the engine computes (sun/moon direction,
> night factor, fog colour, camera altitude, fog range) is *not* an ImGui param and stays on its own
> per-frame channel. Adding a new knob touches only the FFI sub-struct, the one-line translation in
> `EngineWgpu::PushRenderParams`, the ImGui panel, and the shader — **never a new FFI method**.

---

## 0. Where we are today (verified 2026-07-12)

**Five** FFI entry points push global, POD look/tuning parameters, each with its own function, C++ push
helper, and layout guard. (The 2026-07-08 draft listed four; `wgr_terrain_set_sky_visibility` has since
landed and belongs in the same consolidation.)

| FFI fn | Payload | Push cadence today |
|---|---|---|
| `wgr_set_tonemap` ([ffi.rs:1370](../rust/src/ffi.rs#L1370)) | `WgrTonemap` (48 B, [ffi.rs:267](../rust/src/ffi.rs#L267)) | init + on edit + **every frame in auto mode** (`UpdateAutoTonemap`) |
| `wgr_set_exposure` ([ffi.rs:1384](../rust/src/ffi.rs#L1384)) | `WgrExposure` (32 B, [ffi.rs:307](../rust/src/ffi.rs#L307)) | init + on edit |
| `wgr_set_sky` ([ffi.rs:1414](../rust/src/ffi.rs#L1414)) | `WgrSky` (176 B, [ffi.rs:342](../rust/src/ffi.rs#L342)) | **every frame** (celestial refresh) + on edit |
| `wgr_terrain_set_sun_shadow` ([ffi.rs:1242](../rust/src/ffi.rs#L1242)) | 4 loose scalars | on edit, **with mask realloc (on `scale` change) / heightfield re-sweep** |
| `wgr_terrain_set_sky_visibility` ([ffi.rs:1267](../rust/src/ffi.rs#L1267)) | 7 loose args | on edit, **with CPU horizon-scan rebuild when radius/azimuths/downsample change** |

The flow is two-layered and stays that way:

1. **Backend-agnostic edit layer** — [Engine.hpp](../../Poseidon/Graphics/Core/Engine.hpp) defines
   `TonemapSettings` / `ExposureSettings` / `SkySettings` / `ExposureSettings` and the sun-shadow +
   sky-visibility fields inside `ShadowMapTuning`, plus `Get*/Set*` virtuals; GL33 leaves the
   `Supports*` gates false. The ImGui panel ([DebugOverlay.cpp](../../Poseidon/Dev/Debug/DebugOverlay.cpp))
   edits **these**. This layer is untouched — the debug panel must **not** couple to wgpu FFI types
   (GL33 must still compile against the neutral interface).
2. **Translation layer** — [EngineWgpu.cpp](../EngineWgpu.cpp) converts the engine settings into the FFI
   `Wgr*` structs and calls the setters: `PushTonemap` ([EngineWgpu.cpp:2510](../EngineWgpu.cpp#L2510)),
   `PushExposure` ([:2542](../EngineWgpu.cpp#L2542)), `PushSky` ([:2602](../EngineWgpu.cpp#L2602)), and
   the inline sun-shadow + sky-visibility pushes in `SetShadowMapTuning`
   ([EngineWgpu.hpp:164](../EngineWgpu.hpp#L164)).

This refactor consolidates only the **FFI boundary** of those five setters.

### The one design change vs the 2026-07-08 draft: split the sky's runtime out

`WgrSky` bundles **authored look** (rayleigh/mie/ozone/turbidity/ground/sun radius+intensity/exposure/
planet+atmosphere geometry/night colours+band/samples/haze/aerial-shadow) with **per-frame runtime**
the engine recomputes each frame from `LightSun` and the camera:

- `sun_dir.xyz` (dir to sun), `moon_dir.xyzw` (dir to moon + phase), `ground_albedo.w` (night factor),
  `fog_color.xyz` (from `FogColor()`), `night_zenith.w` (camera altitude ASL), `night_params.w` (fog
  far-range). These are **not** ImGui knobs.

Rather than cram runtime into `WgrRenderParams` and push the whole block every frame (the old draft's
approach), the runtime fields move to a small per-frame `WgrSkyRuntime` pushed via `wgr_set_sky_runtime`.
`WgrRenderParams.sky` carries only the authored look. **The `WgrSky` UBO layout and the sky shader do not
change** — the renderer keeps one `sky_params: WgrSky` and we merely split its *writers*:
`set_render_params` writes the look subset, `set_sky_runtime` writes the runtime subset. Same bytes reach
the GPU; the concern is now cleanly divided at the FFI.

## 1. Target FFI shape

Add to both [wgpu_renderer.hpp](../include/wgpu_renderer.hpp) and [ffi.rs](../rust/src/ffi.rs). `WgrTonemap`
and `WgrExposure` are nested **unchanged** (their tonemap/exposure UBO layouts are untouched). `WgrSky` is
**replaced at the FFI setter boundary** by `WgrSkyLook` (authored fields only) + `WgrSkyRuntime` (runtime
fields only); the UBO-facing `WgrSky` remains as the renderer's internal assembly target. The two loose
terrain setters become POD sub-structs.

```cpp
/* Authored procedural-sky look (the ImGui Sky tab). No celestial/runtime fields — those ride
   WgrSkyRuntime. The renderer folds these into the WgrSky UBO's look slots. */
struct WgrSkyLook
{
    WgrVec4 rayleigh;      /* xyz = scattering coeff (1/m); w = scale height (m) */
    WgrVec4 mie;           /* x = coeff, y = g, z = scale height (m), w = turbidity */
    WgrVec4 ground_sun;    /* xyz = ground albedo; w = sun radiance scale (sunIntensity) */
    WgrVec4 params;        /* x = sun angular radius (rad), y = exposure, z = planet radius (m), w = atmosphere (m) */
    WgrVec4 control;       /* x = enabled, y = view samples, z = light samples, w = ozone */
    WgrVec4 night_zenith;  /* xyz = night radiance at the zenith; w = horizon-haze strength */
    WgrVec4 night_horizon; /* xyz = night radiance at the horizon; w = aerial-shadow strength */
    WgrVec4 night_params;  /* x = full-day sun_dir.y, y = full-night sun_dir.y, z = night intensity, w = pad */
};

/* Per-frame celestial + camera runtime (recomputed from LightSun / the camera). NOT an ImGui knob;
   pushed every frame via wgr_set_sky_runtime. Folded into the WgrSky UBO's runtime slots. */
struct WgrSkyRuntime
{
    WgrVec4 sun_dir;   /* xyz = unit dir TO the sun; w = pad */
    WgrVec4 moon_dir;  /* xyz = unit dir TO the moon; w = moon phase */
    WgrVec4 fog_color; /* xyz = scene fog colour; w = fog far-range (m) */
    WgrVec4 misc;      /* x = night factor (0..1), y = camera altitude ASL (m), z/w = pad */
};

/* Long-distance terrain sun-shadow sweep (was wgr_terrain_set_sun_shadow's args). */
struct WgrTerrainSunShadow
{
    float    strength;     /* 0 = disabled */
    uint32_t scale;        /* mask supersample factor — CHANGING THIS reallocates the mask */
    uint32_t max_steps;    /* march cap (steps * terrain_grid) */
    float    penumbra_deg; /* soft-edge half-width */
};

/* Terrain sky-visibility (sky-view factor) AO (was wgr_terrain_set_sky_visibility's args). */
struct WgrSkyVisibility
{
    float    strength;   /* 0 = disabled */
    float    contrast;   /* deepens the near-1 factor */
    float    floor;      /* minimum ambient in fully-occluded columns */
    float    radius_m;   /* horizon-scan reach (m) — CHANGING re-runs the CPU scan */
    uint32_t k_azimuths; /* scan direction count — CHANGING re-runs the scan */
    uint32_t downsample; /* scan coarseness — CHANGING re-runs the scan */
    uint32_t debug;      /* 1 = terrain outputs the factor as greyscale */
    uint32_t _pad;
};

/* Every imgui-tweakable render parameter that crosses the FFI as a setter, pushed as one block via
   wgr_set_render_params. Append future look knobs here; do not add new FFI setters. */
struct WgrRenderParams
{
    WgrTonemap          tonemap;
    WgrExposure         exposure;
    WgrSkyLook          sky;
    WgrTerrainSunShadow terrain_sun_shadow;
    WgrSkyVisibility    sky_visibility;
};

WGR_API void wgr_set_render_params(WgrRenderer*, const WgrRenderParams*);
WGR_API void wgr_set_sky_runtime(WgrRenderer*, const WgrSkyRuntime*);
```

`WgrRenderParams` is passed only by pointer (never uploaded whole to a GPU buffer), so it needs `#[repr(C)]`
but not `Pod`/`Zeroable`; the nested structs keep/get their existing derives. Give the Rust
`WgrRenderParams` and every new sub-struct a `Default` (composing the sub-struct defaults, and the sky look
default matching today's `WgrSky::default()` look fields). Extend the `const _: () = assert!(size_of…)`
guards in [ffi.rs](../rust/src/ffi.rs#L684) and the matching `static_assert`s in
[wgpu_renderer.hpp](../include/wgpu_renderer.hpp#L702):

- `WgrSkyLook` = 8 vec4 = **128 B**
- `WgrSkyRuntime` = 4 vec4 = **64 B**
- `WgrTerrainSunShadow` = **16 B**
- `WgrSkyVisibility` = 8×4 B = **32 B**
- `WgrRenderParams` = 48 + 32 + 128 + 16 + 32 = **256 B**

## 2. Rust application + the one correctness subtlety

`wgr_set_render_params` copies the struct in (null-guarded, `catch_unwind`, same shape as the existing
setters) and calls a new `Renderer::set_render_params`, which **fans out to the existing private methods**:

- `tonemap` → `self.tonemap_params = p.tonemap` (was `set_tonemap`)
- `exposure` → `self.exposure_params = p.exposure` (was `set_exposure`)
- `sky` → write the **look subset** of `self.sky_params` (the WgrSky UBO), leaving the runtime slots that
  `set_sky_runtime` owns untouched
- `terrain_sun_shadow` → `terrain.set_sun_shadow_params(...)`
- `sky_visibility` → `terrain.set_sky_visibility(...)`

`wgr_set_sky_runtime` writes the **runtime subset** of `self.sky_params` (sun/moon dir + phase, night
factor, fog colour, cam altitude, fog far-range).

**The one load-bearing behavioral requirement:** blind per-frame push is correct and cheap for tonemap /
exposure / sky-look — they re-upload their UBOs each frame or on change regardless. It is **not** cheap for
the two terrain setters: `set_sun_shadow_params` sets `shadow_dirty = true` on every call (re-running the
amortized sweep), and `set_sky_visibility` rebuilds the CPU scan when its shape args change. `set_render_params`
is called every frame (auto-ToD mutates the look each frame). So `set_render_params` **must store the
last-received `WgrTerrainSunShadow` + `WgrSkyVisibility`, compare, and only fan out to the terrain methods
when they actually differ.** Everything else is a mechanical fold. (The terrain methods already diff their
*expensive* realloc/scan internally — `scale` for the mask, `radius/azimuths/downsample` for the scan — but
`set_sun_shadow_params` still dirties the sweep unconditionally, so the diff must live in `set_render_params`.)

Keep the private `set_tonemap` / `set_exposure` methods only if still convenient; the fan-out can inline the
assignments. `terrain_set_sun_shadow` / `terrain_set_sky_visibility` (the `Renderer` methods) stay.

## 3. C++ bridge changes

- EngineWgpu keeps `_tonemap` / `_exposure` / `_sky` / `_smTuning` as the edit source of truth (ImGui still
  edits via the unchanged `Set*Settings` / `SetShadowMapTuning` virtuals).
- Add `PushRenderParams()` — assembles a stack `WgrRenderParams`, lifting the existing translation code
  verbatim from `PushTonemap` ([EngineWgpu.cpp:2514](../EngineWgpu.cpp#L2514)), `PushExposure`
  ([:2546](../EngineWgpu.cpp#L2546)), the **look** half of `PushSky` ([:2656](../EngineWgpu.cpp#L2656)),
  and the sun-shadow + sky-visibility clamps from `SetShadowMapTuning`
  ([EngineWgpu.hpp:171](../EngineWgpu.hpp#L171)) — and calls `wgr_set_render_params`.
- Add `PushSkyRuntime()` — the **celestial/runtime** half of today's `PushSky` (the eased
  `_skySunDir`/`_skyMoonDir`/`_skyNight`/`_skyFog` state, `camAlt`, `fogFar`) assembled into a
  `WgrSkyRuntime` and pushed via `wgr_set_sky_runtime`.
- Replace `PushTonemap` / `PushExposure` / `PushSky` and the inline
  `wgr_terrain_set_sun_shadow` / `wgr_terrain_set_sky_visibility` calls in `SetShadowMapTuning`.
- Every former push site calls the new helpers:
  - `SetTonemapSettings` / `SetExposureSettings` / `SetSkySettings` / `SetShadowMapTuning` edits →
    `PushRenderParams()`.
  - `NextFrame` (per frame): `UpdateAutoTonemap` + `UpdateAutoSky` mutate the look, then
    `PushRenderParams()` (the §2 diff absorbs the terrain cost) + `PushSkyRuntime()`.
- `wgr_get_exposure_scale` stays a separate readback (it is not a param).

## 4. Migration order (each step compiles)

1. Add `WgrSkyLook` / `WgrSkyRuntime` / `WgrTerrainSunShadow` / `WgrSkyVisibility` / `WgrRenderParams` +
   `wgr_set_render_params` + `wgr_set_sky_runtime` on both sides of the FFI, with layout guards. Rust impl
   fans out to the existing methods with the §2 terrain diff and the sky look/runtime writer split. Old
   setters still present.
2. Switch EngineWgpu to `PushRenderParams()` + `PushSkyRuntime()`; delete `PushTonemap` / `PushExposure` /
   `PushSky` and the inline terrain calls.
3. Once no caller remains, delete `wgr_set_tonemap`, `wgr_set_exposure`, `wgr_set_sky`,
   `wgr_terrain_set_sun_shadow`, `wgr_terrain_set_sky_visibility` and their `.hpp` declarations (and, if now
   unused, the private `set_tonemap`/`set_exposure`/`set_sky` Rust methods).

## 5. Explicitly out of scope

- **Water look + terrain wet-band (`WaterSettings`).** These *are* ImGui knobs (the Water tab), but they do
  **not** cross the FFI as a look-setter: `WaterWgpu::DrawWater` and `TerrainWgpu` read `EngineWgpu::WaterLook()`
  on the C++ side and **merge it with per-frame runtime** (sea level, time, swash, world placement) into the
  `WgrWaterParams` / `WgrTerrainParams` UBOs, pushed each frame by `wgr_water_set_params` /
  `wgr_terrain_set_params` ([WaterWgpu.cpp:165](../WaterWgpu.cpp#L165),
  [TerrainWgpu.cpp:249](../TerrainWgpu.cpp#L249)). Look and runtime are inseparable in those small
  per-frame UBOs and the merge already happens engine-side — routing the look through `WgrRenderParams`
  would split it away from the runtime it must ship with, for no gain. They stay as per-frame merged UBOs
  owned by their sub-renderers (which is exactly the "runtime updated each frame stays separate" principle).
- **Resource uploads** (variable-length data, not params): heightmap, ground layers, index/jitter maps,
  detail layer, mesh/texture create/update.
- **Readbacks:** `wgr_get_exposure_scale`, `wgr_shadow_map_read`, `wgr_shadow_depth_probe`.
- **Per-frame camera/shadow blocks** (`WgrCamera` / `WgrCameraShadow` / `WgrShadowPass`): runtime, not
  ImGui setters. The CSM *tuning* fields in `ShadowMapTuning` reach the GPU through the per-frame cascade
  build (`SetShadowCascades`), not a setter, so they are not part of this consolidation.
