# WTR-003 — Water debug views

## Summary

Implemented switchable on-surface debug diagnostics for the GPU water pipeline.
A new `WgrWaterDebugView` selector (0–36) is written to `WgrWaterParams.debug_params.x`
each frame; when non-zero the water fragment shader replaces its lit output with the
chosen diagnostic. The Water tab gains a "Debug views (WTR-003)" combo (37 entries,
index-matched to the enum). Views 1–29 are backed by live pipeline data; views 30–36
are reserved (underwater froxel, god-rays, caustics, whitewater — no backing pass yet)
and render black.

## View coverage

| # | View | Data source |
|---|------|-------------|
| 1 | FFT displacement | `fft_displacement.xyz` summed over 4 cascades |
| 2 | FFT horizontal | `length(disp.xz)` |
| 3 | FFT vertical | `disp.y` (signed heatmap) |
| 4 | FFT slope | `fft_dynamics.xy` magnitude |
| 5 | Jacobian | `fft_auxiliary.x` (min over cascades) |
| 6 | Compression | `fft_auxiliary.y` (max) |
| 7 | Curvature | `fft_auxiliary.z` (max) |
| 8 | Crest energy | `fft_displacement.w` (max) |
| 9 | Slope variance | `fft_auxiliary.w` (sum) |
| 10 | Material coordinate | `fract(base_xz * 0.01)` |
| 11 | Displaced world coordinate | `fract((base_xz + world_rel.xz) * 0.01)` |
| 12 | Interaction height | `interaction_field.r` (signed) |
| 13 | Interaction velocity | `interaction_field.g` (signed) |
| 14 | Interaction foam/aeration | `interaction_field.b` |
| 15 | Persistent foam source | breaker gates (same thresholds as `foam.wgsl`) + aeration |
| 16 | Persistent foam history | `foam_history.r` coverage |
| 17 | Surface velocity | `interaction_field.g` mapped to grey |
| 18 | Water-column depth | `seabed_depth()` (0–60 m ramp) |
| 19 | Camera-to-surface distance | `length(world_rel)` (0–1000 m ramp) |
| 20 | SSR colour | `reflected_scene().rgb` |
| 21 | SSR confidence | `reflected_scene().a` |
| 22 | Planar colour | `planar_reflection().rgb` |
| 23 | Planar geometry validity | `planar_reflection().a` |
| 24 | Directional sky/cloud refl. | `sky_env_sample(reflect_dir)` |
| 25 | Reflection-source selection | colour-coded: red=SSR, blue=planar, green=sky |
| 26 | Refraction ray | UV offset in pixel space (±32 px mapped to RGB) |
| 27 | Refraction hit validity | `refracted_scene().valid` |
| 28 | Refraction path length | column depth proxy (0–40 m ramp) |
| 29 | RGB transmittance | `transmission * 1.4` |
| 30–36 | Reserved | black (no backing pass) |

## Design

- **ABI**: `WgrVec4 debug_params` appended to `WgrWaterParams` (192 → 208 bytes).
  Static asserts on both sides (C++ `static_assert`, Rust `const _` + `offset_of!` test).
- **Shader**: `debug_view()` function + `dbg_heat`/`dbg_signed` colour helpers.
  Early-return in `fs_water` after all intermediates are computed (line ~742), before
  specular/foam/fog — debug output is raw, un-fogged, alpha = 1.
- **UI**: `Engine::WaterSettings::debugView` (int, default 0). Water tab combo outside
  the `BeginDisabled` block so it works even with the water surface toggled off.
- **Backend-agnostic**: `debugView` lives in the engine-layer `WaterSettings`; non-wgpu
  backends ignore it. The enum + count live in `wgpu_renderer.hpp`; the overlay uses a
  hard-coded count (37) to avoid an engine → renderer ABI dependency.

## Files changed

| File | Change |
|------|--------|
| `engine/WgpuRenderer/include/wgpu_renderer.hpp` | `debug_params` field, `WgrWaterDebugView` enum, sizeof 208 |
| `engine/WgpuRenderer/rust/src/ffi.rs` | `debug_params: WgrVec4`, size assert 208 |
| `engine/WgpuRenderer/rust/src/water/mod.rs` | `default_params.debug_params`, offset test |
| `engine/WgpuRenderer/rust/src/water/water.wgsl` | `debug_params` struct field, `debug_view()` + helpers, early-return branch |
| `engine/Poseidon/Graphics/Core/Engine.hpp` | `WaterSettings::debugView` |
| `engine/WgpuRenderer/WaterWgpu.cpp` | `_params.debug_params = {debugView, 0, 0, 0}` |
| `engine/Poseidon/Dev/Debug/DebugOverlay.cpp` | "Debug views (WTR-003)" combo (37 entries) |

## Tests

- `cargo test --package wgpu_renderer --profile rwdi`: **45 passed**, 0 failed, 1 ignored.
  - New: `debug_params_appended_without_shifting_existing_lanes` (offset_of! 192, size 208).
  - Shader validation: `entry_shaders_compose` (naga) validates the new WGSL at compile time.
    Caught a real bug during development (vec3 compose with 2 components in case 17).

## Build

- `cmake --build build/win-x64-clang-rwdi --target PoseidonGame`: linked (196/197), deps deployed.
- NOTE: the CMake deploy step still stages a **stale** `wgpu_renderer.dll` in `apps\cwr\Game\`
  (same issue as WTR-002). The fresh dll is the corrosion output at
  `build/win-x64-clang-rwdi/engine/WgpuRenderer/wgpu_renderer.dll`. Always deploy that one.

## Smoke test

Two consecutive `--check --render=wgpu` runs against the Steam install (RTX 3070, Vulkan):

- ExitCode = 0, stderr = 0 bytes, ERRR lines = 0 (both runs).
- Log confirms: `wgpu: WTR-002 GPU timestamp instrumentation enabled` (fresh dll loaded),
  `wgpu: Hydro FFT ocean enabled`, `wgpu renderer created`.
- The water pipeline (including the new `debug_view` WGSL) validated at pipeline creation.

## Known limitations

- **Visual confirmation pending**: `--check` never opens the Water tab or sets debugView ≠ 0,
  so the debug views haven't been visually confirmed on-screen yet. Needs an interactive
  session (Ctrl+\` → Water tab → Debug views combo).
- **Reserved views (30–36)** render black until underwater/god-ray/caustic/whitewater passes
  are implemented (WTR-005+).
- **FFT views aggregate all 4 cascades** — no per-cascade isolation yet (`debug_params.y`
  is reserved for a future cascade selector).
- **Interaction/foam views read zero** outside the 256 m camera-relative domain.

## Next task

**WTR-004 — Standard test scenes** (reproducible scenes: seabed checkerboard, slow orbit,
controlled lighting for before/after shader comparisons).
