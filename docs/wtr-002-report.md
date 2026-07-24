# WTR-002 — GPU timestamp instrumentation

Plan reference: `.agents/CWR-CE Water System Master Plan.md` §5 Phase WTR-000.

## Task ID
WTR-002 (only); WTR-003/004 left out per the plan's "one principal concern per PR" rule.

## Single objective
Add GPU timestamp-query brackets around every named water-pipeline region so per-pass
timings are visible in the Water tab before any FFT rework begins (the plan's hard gate:
"Do not replace the FFT implementation before these timings exist").

## Region coverage vs. the spec list
| Spec row | Region index | Status |
|----------|-------------|--------|
| Spectrum generation | 0 `SPECTRUM_INIT` | measured (spectrum-dirty frames only) |
| Spectrum evolution | 1 `SPECTRUM_EVOLVE` | measured |
| FFT horizontal stages | 2 `FFT_HORIZONTAL` | measured |
| FFT vertical stages | 3 `FFT_VERTICAL` | measured |
| FFT composition | 4 `FFT_COMPOSE` | measured |
| Interaction injection | 5 `INTERACTION` | measured — injection + propagation are ONE fused kernel today, so a single bracket covers both spec rows (the split lands with the interaction rework) |
| Interaction propagation | 5 `INTERACTION` | (same bracket, see above) |
| Foam update | 6 `FOAM` | measured |
| Whitewater update | 7 `WHITEWATER` | reserved (-1 "n/a") — no whitewater pass exists on the fork yet |
| Planar sky | 8 `PLANAR_SKY` | measured |
| Planar terrain | 9 `PLANAR_TERRAIN` | measured |
| Planar objects | 10 `PLANAR_OBJECTS` | measured (bracket includes the reflected-view cull dispatch) |
| Planar clouds | 11 `PLANAR_CLOUDS` | measured (cloud march + composite) |
| Planar mip generation | 12 `PLANAR_MIPS` | measured |
| Water SSR | 13 `WATER_SSR` | reserved — SSR is fragment work inside the water draw; its cost is inside `WATER_DRAW` |
| Water refraction | 14 `WATER_REFRACTION` | reserved — same, in-shader inside `WATER_DRAW` |
| Water draw | 15 `WATER_DRAW` | measured (includes SSR + refraction cost) |
| Underwater froxel generation | 16 `UNDERWATER_FROXEL` | reserved — no froxel pass exists yet |
| Underwater composite | 17 `UNDERWATER_COMPOSITE` | measured at all three call sites (tonemap seam, resolve else-branch, end-of-frame fallback) |
| Caustic generation | 18 `CAUSTICS` | reserved — caustics are evaluated inside the underwater compositor / water shaders |

Reserved rows hold fixed FFI indices reporting -1 ms ("n/a") so the ABI + Water-tab rows
are already in place when those passes land (append-only contract, locked by a Rust test).

## Design
* `TIMESTAMP_QUERY` + `TIMESTAMP_QUERY_INSIDE_ENCODERS`, adapter-gated like
  `partially_bound`: absent features ⇒ inert timers, FFI returns 0, tab shows "Unavailable".
* Brackets are written on the COMMAND ENCODER between passes (no pass-descriptor edits),
  so one bracket can cover multi-pass regions (planar clouds, the mip chain).
* `begin`/`end` take `&self` (written mask in a `Cell`) so brackets drop into
  `render_frame` beside the existing disjoint field borrows.
* Readback never blocks: resolve + copy ride the frame encoder into a 3-slot round-robin
  ring; `map_async` after submit; drained with a non-blocking `PollType::Poll` the next
  frame. A saturated ring drops that frame's sample.
* The WTR-001 freeze mask composes naturally: a frozen dispatch writes no bracket that
  frame, so its row simply holds its last measured value.

## Files changed
| File | Change |
|------|--------|
| `engine/WgpuRenderer/rust/src/gpu_timers.rs` | NEW — `GpuTimers` (query set, readback ring, harvest, region contract + 2 unit tests). |
| `engine/WgpuRenderer/rust/src/water/fft.rs` | Split the single "wgr_water_fft" compute pass into 5 separately-bracketed passes (spectrum_init / spectrum_evolve / horizontal / vertical / compose). Same pipelines, same dispatch math — only pass boundaries moved. |
| `engine/WgpuRenderer/rust/src/water/mod.rs` | `update_interactions` takes `timers`; Interaction + Foam brackets (freeze-skip preserved). |
| `engine/WgpuRenderer/rust/src/lib.rs` | Feature gating in `Renderer::new` + init log; `gpu_timers` field; `begin_frame`/brackets/`resolve`/`harvest` in `render_frame`; `gpu_timings()` accessor. |
| `engine/WgpuRenderer/rust/src/ffi.rs` | `wgr_get_gpu_timings(renderer, out_ms, out_len) -> u32` (null-checked, catch_unwind). |
| `engine/WgpuRenderer/include/wgpu_renderer.hpp` | `WgrGpuTimerRegion` enum (the index contract) + the `wgr_get_gpu_timings` prototype. |
| `engine/Poseidon/Graphics/Core/Engine.hpp` | `GetWaterGpuTimings` / `GetWaterGpuTimingName` virtuals, appended at the class end per the vtable-slot note (inert defaults for non-wgpu backends). |
| `engine/WgpuRenderer/EngineWgpu.hpp` / `.cpp` | Overrides calling `wgr_get_gpu_timings` + the 19-entry name table ordered by `WgrGpuTimerRegion`. |
| `engine/Poseidon/Dev/Debug/DebugOverlay.cpp` | "GPU timings (WTR-002)" section at the end of the Water tab: per-region table, "n/a" for reserved rows, measured-total footer. |

No FFI struct changes; one new export. The C ABI is a flat `float*` copy indexed by the
enum, so adding future regions is append-only on both sides.

## Bug found and fixed during smoke testing
First smoke run surfaced a device loss (`get_current_texture: validation error`, invalid
`wgr_texture`, and a shutdown panic `Buffer 'wgr_gpu_timers_readback_0' has been
destroyed`). Root cause: `resolve()` resolved the full `0..38` query range, but queries
never written that frame (reserved regions, spectrum-init after frame 1) are never
reset/made *available* on Vulkan — resolving unavailable queries wedges the device.
Fix: before the full-range resolve, stamp every unwritten pair with a dummy timestamp so
all 38 queries are always available; harvest ignores the dummies via the written mask.
After the fix, two consecutive `--check` runs are completely clean (0 ERRR lines, empty
stderr).

## Tests added
* `gpu_timers::tests::region_indices_stay_ffi_stable` — pins REGION_COUNT=19 plus six
  representative discriminants; guards the C++ enum / name-table ordering.
* `gpu_timers::tests::disabled_timers_report_zero_regions` — a feature-less adapter must
  be a total no-op that reports 0 regions through the FFI.

## Build commands and results
* `cargo test --package wgpu_renderer --profile rwdi`
  Result: **44 passed; 0 failed; 1 ignored** (42 pre-existing + 2 new).
* `cmake --build build/win-x64-clang-rwdi --target PoseidonGame`
  Result: clean link; `wgpu_renderer.dll` rebuilt with the new export.
  NOTE: CMake's dependency-deploy step left a stale (Jul 21) `wgpu_renderer.dll` in
  `apps/cwr/Game/`; the freshly built dll lives in
  `build/win-x64-clang-rwdi/engine/WgpuRenderer/` and was copied over manually (verified
  by scanning for the `wgr_get_gpu_timings` export).

## Smoke test (per the standing instruction: overwrite the steam exe with the fork)
Copied the fork `PoseidonGame.exe` over
`D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe` plus the fresh
`wgpu_renderer.dll`, ran `ColdWarAssault.exe --check --render=wgpu` against the real game
data. Result (after the resolve fix, two consecutive runs):
* Exit code **0**, zero `ERRR` log lines, empty stderr.
* Log confirms the feature path: `wgpu: WTR-002 GPU timestamp instrumentation enabled`
  (RTX 3070 / Vulkan exposes both timestamp features), renderer created, world
  initialized, "Initialization check complete - exiting".
The fork exe + dll remain in place in the Steam dir (standing instruction);
`ColdWarAssault-vanilla.exe` / `*.stock.bak` backups are untouched for rollback.

## Known limitations
* `--check` never opens the Water tab, so the end-to-end number display was exercised only
  by code inspection + the FFI contract tests; visual confirmation of live ms values needs
  an interactive session (open the debug overlay → Water tab → "GPU timings (WTR-002)").
* Encoder-level timestamps measure GPU queue time between brackets; overlapping/async GPU
  work can make the per-region sum exceed the pipeline's wall-clock cost (documented in
  the tab's tooltip).
* The interaction row covers injection + propagation together (one fused kernel today);
  when the interaction rework splits the kernel, the fused row keeps index 5 and the new
  rows append fresh indices per the append-only rule.
* Timings lag the displayed frame by the readback ring depth (~2-3 frames) — fine for the
  profiling gate, not a per-frame exactness tool.

## Next task
WTR-003 — water debug visualization modes (per plan §5), building on WTR-001 determinism +
these timings as the before/after basis.
