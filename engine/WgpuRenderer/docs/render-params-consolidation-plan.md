# Plan: Consolidate the imgui-tweakable render params into one `WgrRenderParams` struct

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** PLANNED (2026-07-08). Concrete.
**Scope:** FFI ergonomics only — no shader, UBO, or visual change.

> **Governing rule:** every ImGui-tweakable render parameter flows through a single monolithic
> `WgrRenderParams` struct (a struct of structs) pushed via one `wgr_set_render_params` call. Adding a
> new knob touches only the FFI sub-struct, the one-line translation in `EngineWgpu::PushRenderParams`,
> the ImGui panel, and the shader — **never a new FFI method**. This plan folds the four existing
> imgui-tweak setters into that shape; every future knob follows the same rule.

---

## 0. Where we are today (verified 2026-07-08)

Four FFI entry points push global, POD look/tuning parameters, each with its own function, C++ push
helper, and layout guard:

| FFI fn | Payload | Push cadence today |
|---|---|---|
| `wgr_set_tonemap` ([ffi.rs:1030](../rust/src/ffi.rs#L1030)) | `WgrTonemap` (48 B, [ffi.rs:197](../rust/src/ffi.rs#L197)) | per-frame in auto mode + on edit |
| `wgr_set_exposure` ([ffi.rs:1044](../rust/src/ffi.rs#L1044)) | `WgrExposure` (32 B, [ffi.rs:237](../rust/src/ffi.rs#L237)) | init + on edit |
| `wgr_set_sky` ([ffi.rs:1074](../rust/src/ffi.rs#L1074)) | `WgrSky` (176 B, [ffi.rs:272](../rust/src/ffi.rs#L272)) | **every frame** (celestial refresh) + on edit |
| `wgr_terrain_set_sun_shadow` ([ffi.rs:932](../rust/src/ffi.rs#L932)) | 4 loose scalars | on edit, **with mask realloc / heightfield re-sweep on change** |

The flow is two-layered and stays that way:

1. **Backend-agnostic edit layer** — [Engine.hpp](../../Poseidon/Graphics/Core/Engine.hpp) defines
   `TonemapSettings` / `ExposureSettings` / `SkySettings` / `ShadowMapTuning` + `Get*/Set*` virtuals;
   GL33 leaves the `Supports*` gates false. The ImGui panel
   ([DebugOverlay.cpp](../../Poseidon/Dev/Debug/DebugOverlay.cpp)) edits **these**.
2. **Translation layer** — [EngineWgpu.cpp](../EngineWgpu.cpp) converts the engine settings into the FFI
   `Wgr*` structs and calls the setters: `SetTonemapSettings`
   ([EngineWgpu.cpp:1750](../EngineWgpu.cpp#L1750)), `SetExposureSettings`
   ([:1777](../EngineWgpu.cpp#L1777)), `PushSky` ([:1900](../EngineWgpu.cpp#L1900)), and the inline
   sun-shadow push in `SetShadowMapTuning` ([EngineWgpu.hpp:134](../EngineWgpu.hpp#L134)).

Both layers are kept — the debug panel must **not** couple to wgpu FFI types (GL33 must still compile
against the neutral interface). This refactor consolidates only the **FFI boundary**.

## 1. Target FFI shape

Nest the existing FFI structs **unchanged** (so their shader/UBO layouts are untouched) plus one new
wrapper for the loose sun-shadow scalars. Add to both [wgpu_renderer.hpp](../include/wgpu_renderer.hpp)
and [ffi.rs](../rust/src/ffi.rs):

```cpp
/* Long-distance terrain sun-shadow sweep knobs (was wgr_terrain_set_sun_shadow's args). */
struct WgrTerrainSunShadow
{
    float    strength;     // 0 = disabled
    uint32_t scale;        // mask supersample factor — CHANGING THIS reallocates the mask
    uint32_t max_steps;    // march cap (steps * terrain_grid)
    float    penumbra_deg; // soft-edge half-width
};

/* Every imgui-tweakable render parameter, pushed as one block via wgr_set_render_params.
   Append future look knobs here; do not add new FFI setters. */
struct WgrRenderParams
{
    WgrTonemap          tonemap;
    WgrExposure         exposure;
    WgrSky              sky;
    WgrTerrainSunShadow terrain_sun_shadow;
};

WGR_API void wgr_set_render_params(WgrRenderer*, const WgrRenderParams*);
```

`WgrRenderParams` is passed only by pointer (never uploaded whole to a GPU buffer), so it needs
`#[repr(C)]` but not `Pod`/`Zeroable`; the nested structs keep their existing derives. Give the Rust
`WgrRenderParams` and `WgrTerrainSunShadow` a `Default` (composing the sub-struct defaults). Extend the
`const _: () = assert!(size_of…)` guards in [ffi.rs](../rust/src/ffi.rs#L567) and the matching
`static_assert`s in [wgpu_renderer.hpp](../include/wgpu_renderer.hpp#L576) for both new types
(`WgrTerrainSunShadow` = 16 B; `WgrRenderParams` = 48 + 32 + 176 + 16 = **272 B**).

## 2. Rust application + the one correctness subtlety

`wgr_set_render_params` copies the struct in (null-guarded, `catch_unwind`, same shape as the existing
setters) and calls a new `Renderer::set_render_params`, which **fans out to the existing private
methods** — `set_tonemap`, `set_exposure`, `set_sky`, `terrain_set_sun_shadow` (in `lib.rs`). Keep those
methods; only their FFI wrappers go away.

Blind per-frame push is correct and cheap for tonemap / exposure / sky — they re-upload their UBOs each
frame or on change regardless. It is **not** cheap for `terrain_sun_shadow`: today the per-arg setter
reallocates the mask when `scale` changes and re-runs the amortized heightfield sweep on *any* change.
So `set_render_params` **must store the last-received `WgrTerrainSunShadow`, compare, and only call
`terrain_set_sun_shadow` when it actually differs.** This is the single load-bearing behavioral
requirement of the whole refactor — everything else is a mechanical fold. (Equivalently: keep
`terrain_set_sun_shadow` internally idempotent/diffing. Storing the last value in `set_render_params` is
the smaller change.)

## 3. C++ bridge changes

- EngineWgpu keeps `_tonemap` / `_exposure` / `_sky` as the edit source of truth (ImGui still edits via
  the unchanged `Set*Settings` virtuals).
- Replace `PushTonemap` / `PushExposure` / `PushSky` ([EngineWgpu.hpp:194](../EngineWgpu.hpp#L194)) and
  the inline `wgr_terrain_set_sun_shadow` call in `SetShadowMapTuning`
  ([EngineWgpu.hpp:134](../EngineWgpu.hpp#L134)) with a single `PushRenderParams()` that assembles a
  stack `WgrRenderParams` — lifting the existing translation code verbatim
  ([tonemap :1760](../EngineWgpu.cpp#L1760), [exposure :1792](../EngineWgpu.cpp#L1792),
  [sky :1902](../EngineWgpu.cpp#L1902), plus the sun-shadow clamp from
  [EngineWgpu.hpp:141](../EngineWgpu.hpp#L141)) — and calls `wgr_set_render_params`.
- Every former push site now calls `PushRenderParams()`: each `Set*Settings` edit, `SetShadowMapTuning`,
  `UpdateAutoTonemap` / `UpdateAutoSky`, and the per-frame celestial refresh (`NextFrame`). Pushing the
  full block every frame is fine — the sky already did, and §2's diff absorbs the sun-shadow cost.
- `wgr_get_exposure_scale` stays a separate readback (it is not a param).

## 4. Migration order (each step compiles)

1. Add `WgrTerrainSunShadow` + `WgrRenderParams` + `wgr_set_render_params` on both sides of the FFI, with
   layout guards. Rust impl fans out to the existing methods with the §2 sun-shadow diff. Old setters
   still present.
2. Switch EngineWgpu to `PushRenderParams()`; delete `PushTonemap` / `PushExposure` / `PushSky` and the
   inline sun-shadow call.
3. Once no caller remains, delete `wgr_set_tonemap`, `wgr_set_exposure`, `wgr_set_sky`,
   `wgr_terrain_set_sun_shadow` and their `.hpp` declarations.

## 5. Explicitly out of scope

- **Resource uploads** (variable-length data, not params): heightmap, ground layers, index/jitter maps,
  detail layer, mesh/texture create/update.
- **Readbacks:** `wgr_get_exposure_scale`, `wgr_shadow_map_read`, `wgr_shadow_depth_probe`.
- **Water — deferred deliberately.** `wgr_water_set_params` ([ffi.rs:839](../rust/src/ffi.rs#L839)) is
  settings (small POD, **not** a resource upload), but it is engine-driven placement + per-frame
  `sea_level` state, not an imgui knob, and there is **active water work in flight** — this refactor must
  not touch the water FFI. When [water-rendering-plan.md](water-rendering-plan.md) lands its imgui look
  knobs (Gerstner, depth colour, reflection), fold the **whole** water block (placement + sea_level +
  look) into `WgrRenderParams` in one move, so water is never migrated twice.
