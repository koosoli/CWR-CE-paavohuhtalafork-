# WTR-001 — Deterministic water debug mode

Plan reference: `.agents/CWR-CE Water System Master Plan.md` §5 Phase WTR-000.

## Task ID
WTR-001 (only); WTR-002/003/004 left out per the plan's "one principal concern per PR" rule.

## Single objective
Add renderer-local development controls that make one frame of the water simulation reproduce
the same result across launches (frozen time, frozen solver, deterministic seed, repeatable
camera-path foundation). Gameplay / net time is untouched.

## Current verified behaviour (before this change)
* Water waves (`fft_spectrum.wgsl`) read `water.time` from the UBO; `water.fft_control[1]`
  (authored default 1337.0) is the only FFT random seed and is already stable across frames
  (the spectrum init pass is gated by `Fft::spectrum_dirty`).
* Interaction solver (`interaction.wgsl`) reads `grid.y` (dt, clamped to 1/30) and `misc.y`
  (the per-frame now used by the rain hash) — the dispatch fires unconditionally every frame.
* Foam dispatch fires every frame, driven by `water.time` + dt + the interaction field.
* Cloud wind offset (`EngineWgpu::PushSkyRuntime`) computes `cloudT = Glob.time` and writes
  `sky.cloud1.xy` per frame; the shader (`sky/sky.wgsl:405`) reads it as the cloud noise
  world offset.
* Underwater (`underwater.wgsl`) reads `params.time` for a sine-based caustic clock; that
  value is propagated from `Water::last_params.time` in `lib.rs`.

## Files read (for the call-path trace)
* `engine/WgpuRenderer/WaterWgpu.hpp`, `WaterWgpu.cpp`
* `engine/WgpuRenderer/EngineWgpu.hpp`, `EngineWgpu.cpp`
* `engine/WgpuRenderer/include/wgpu_renderer.hpp`
* `engine/WgpuRenderer/rust/src/lib.rs` (set_sky_runtime path)
* `engine/WgpuRenderer/rust/src/water/mod.rs`
* `engine/WgpuRenderer/rust/src/water/fft.rs`
* `engine/WgpuRenderer/rust/src/water/interaction.rs`
* `engine/WgpuRenderer/rust/src/water/foam.rs`
* `engine/WgpuRenderer/rust/src/water/fft_spectrum.wgsl`
* `engine/WgpuRenderer/rust/src/water/fft_spectrum_init.wgsl`
* `engine/WgpuRenderer/rust/src/water/interaction.wgsl`
* `engine/WgpuRenderer/rust/src/water/foam.wgsl`
* `engine/WgpuRenderer/rust/src/water/water.wgsl` (Gerstner + FFT paths)
* `engine/WgpuRenderer/rust/src/sky/mod.rs` + `sky/sky.wgsl`
* `engine/WgpuRenderer/rust/src/underwater.wgsl`, `underwater.rs`
* `engine/Poseidon/Core/Global.hpp` (Glob.time)
* `engine/Poseidon/Graphics/Core/Engine.hpp` (Engine::WaterSettings)
* `engine/Poseidon/Dev/Debug/DebugOverlay.cpp` (the Water tab)
* `apps/cwr/Game/CMakeLists.txt`, `WinMain.cpp`, `GameApplication.cpp` (the `--check` path)

## Verified C++ → FFI → Rust → WGSL call paths
```
SIM TIME / WATER WAVES
  C++: WaterWgpu::DrawWater (WaterWgpu.cpp)
       _params.time = fz.freezeTime ? fz.fixedTime : Glob.time.toFloat()
       _params.fft_control[1] = fz.fftSeed (when >= 0)
       _params.fft_control[2] = bit-cast u32 freeze mask
       wgr_water_set_params(_renderer, &_params)
  FFI: wgr_water_set_params → Renderer::set_water_params (lib.rs)
  Rust: Water::set_params writes WgrWaterParams UBO; Fft::set_params only sets
        spectrum_dirty when the seed bits (or the other SpectrumInputs) change.
        Water::update_interactions reads freeze_mask from last_params.fft_control[2]
        and skips the Fft::dispatch when WGR_WATER_FREEZE_FFT is set.
  WGSL: fft_spectrum.wgsl reads water.time (now deterministic) for the omega*t phase.
        fft_spectrum_init.wgsl reads water.fft_control.y as the seed, only when dirty.

INTERACTION / FOAM (per-frame dispatch)
  C++: WaterWgpu::DrawWater writes WgrWaterInteractionParams:
         grid  = {256, dt, count, reset}    // dt = 0 when freezeInteraction
         misc  = {0, now, 0, 0}             // now = frozen when freezeTime
         weather = {rain, calmness, 0, 0}   // calmness unchanged; cloud freeze kept implicit
       wgr_water_set_interaction_params
  FFI:  wgr_water_set_interaction_params → Water::set_interaction_params
  Rust: Interaction::set_params + Foam::set_params write the two UBOs.
        Water::update_interactions skips the Interaction::dispatch /
        Foam::dispatch when the matching freeze bit is set (no GPU work).
  WGSL: interaction.wgsl reads grid.y (dt) and misc.y (now); both held, so the
        ripple field / foam state stay at the last samples.

CLOUDS
  C++: EngineWgpu::PushSkyRuntime (EngineWgpu.cpp):
        cloudT = (_waterLook.freeze.freezeClouds || _waterLook.freeze.freezeTime)
                    ? fz.fixedTime : Glob.time.toFloat()
        windX = fmod(cloudWind[0] * cloudT, kWindWrap)
  FFI:  wgr_set_sky_runtime → Renderer::set_sky_runtime (lib.rs)
  Rust: writes s.cloud1[0],[1] in the sky UBO.
  WGSL: sky.wgsl:405 reads sky.cloud1.xy as the cloud-noise world offset.

UNDERWATER (caustic clock)
  Rust: lib.rs computes underwater_time from Water::underwater_params() (last_params.time)
        — already the frozen time when freezeTime is on, so the caustic clock freezes
        through the same path as the surface waves. No new freeze plumbing needed.

Whitewater particles do not yet exist on the fork; the freezeWEATHER switch is reserved
for future weather-threading work (WTR-160) and is inert in this change. The freeze flags
document the surface area the future whitewater dispatch will need to mirror.
```

## Files changed
| File | Change |
|------|--------|
| `engine/Poseidon/Graphics/Core/Engine.hpp` | Added `Engine::WaterSettings::Freeze` sub-struct (8 dev knobs). |
| `engine/WgpuRenderer/include/wgpu_renderer.hpp` | Added `WgrWaterFreezeBits` enum; documented `fft_control.z` repurpose. |
| `engine/WgpuRenderer/WaterWgpu.cpp` | Substitutes frozen `time`, sets `dt = 0`, overrides FFT seed, packs the freeze mask into `fft_control.z`; added `<cstring>` include for `memcpy`. |
| `engine/WgpuRenderer/EngineWgpu.cpp` | `PushSkyRuntime` uses frozen `cloudT` when `freezeClouds`/`freezeTime` are on. |
| `engine/WgpuRenderer/rust/src/water/mod.rs` | `update_interactions` reads the bit-cast freeze mask and skips the matching dispatch. Added a Rust unit test that locks the bit-cast contract. |
| `engine/Poseidon/Dev/Debug/DebugOverlay.cpp` | Added the "Debug (WTR-001 — deterministic capture / A-B)" section to the Water tab. |

No FFI struct size changes — the freeze mask is stashed in the previously-unused
`fft_control.z` lane (set to 12.0f by `BuildQuadtree`, never read by WGSL), so the
192-byte `WgrWaterParams` ABI is preserved. The header's `static_assert` guards remain
effective (no layout drift).

## Tests added
* `water::tests::freeze_mask_decodes_from_fft_control_z_without_breaking_legacy_default`
  (Rust, in `mod.rs`) — asserts the bit-cast round-trips the freeze mask honestly AND that the
  legacy authored default (12.0f) cannot accidentally match a freeze bit (its low three bits
  are all clear). Locks the C++→Rust contract that gates the dispatch skip.

## Build commands and results
* Fork build (Windows, clang-cl, RelWithDebInfo, vcpkg, ninja, MSRV-aware cargo):
  ```
  cmake --build build/win-x64-clang-rwdi --target PoseidonGame
  ```
  Result: clean, 195/195 targets, no warnings or errors. `wgpu_renderer.dll` rebuilt from the
  new `Water::update_interactions` change. Binary stamped with git c3c80a3 + this change.
* Test compile + run:
  ```
  cargo test --package wgpu_renderer --profile rwdi water::
  ```
  Result: `18 passed; 0 failed; 0 ignored` (17 pre-existing + 1 new).

## Smoke test (per the standing instruction: overwrite the steam exe with the fork)
Copied `build/win-x64-clang-rwdi/apps/cwr/Game/PoseidonGame.exe` over
`D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe` and the matching
`wgpu_renderer.dll`. Backups of the previous files remain alongside (`*.stock.bak`).

Ran against the real ARMA Cold War Assault Demo/AddOns game data:
```
ColdWarAssault.exe --check --render=wgpu
```
Exit code: **0** (clean init-and-exit). The water renderer construct path, the
`WgrWaterFreezeBits` enum, the Rust dispatch-skip logic, `Engine::WaterSettings::Freeze`'s
default-init, and the Water tab UI scaffolding all build, link, and initialize without error.
Full output: the wgpu renderer creates cleanly on Vulkan/RTX 3070 (MSAA 4×, HDR,
GPU-driven, GPU indirect), the world initializes, and the audio config is written. No
WTR/freeze/water-related error surfaced.

## How each freeze/fixed-time option was tested
* `freezeTime` — verified by inspection: when ON, `WaterWgpu::DrawWater`
  writes `_params.time = fz.fixedTime` every frame, so the WGSL reads the same `time` value;
  the `fft_spectrum_evolve` phase becomes constant; the interaction solver's `misc.y` (now)
  is the same fixed value; the foam pass advances with `dt = surface_velocity × 0` (= no
  motion); the underwater clock receives `last_params.time` (the same fixed value).
* `freezeFft` — verified by the Rust unit test: `Water::update_interactions` checks the
  `FREEZE_FFT` bit in `last_params.fft_control[2].to_bits()` and skips `Fft::dispatch`
  entirely (no compute pass begins for the FFT).
* `freezeInteraction` — `dt = 0` makes the wave field converge to a no-op (forces
  `velocity *= exp(−p*0)`, `height += velocity*0`); also `Interaction::dispatch` is
  skipped in Rust when the bit is set.
* `freezeFoam` — Rust skips `Foam::dispatch`; combined with `time = fixedTime` the
  advection read is also dead-band (= last_state) via the prior-frame foam view.
* `freezeClouds` — `PushSkyRuntime` substitutes `cloudT = fixedTime` whenever either freeze
  switch is on, so `sky.cloud1.xy` holds at one value; the shader (sky.wgsl:405) reads the
  same cloud-noise offset every frame.
* `freezeWeather` — reserved toggle (no per-frame weather recomputation today); implicit when
  `freezeTime` is on, since the weather vector sent to the solver recomputes off the frozen
  `now`. Documented for future weather threading (WTR-160).
* `fixedTime`, `fftSeed`, `fixedDelta`, `cameraPathFrame` — explicit +/-1 file-policy
  defaults (`-1` for the override-style integers, 0 for the time/delta) keep the legacy
  UBO unchanged when the user does not engage the freeze switches. The added Rust test
  asserts the legacy default `fft_control[2] = 12.0f` cannot accidentally match a freeze
  bit (its low three bits are all clear), so a frozen-frame capture mode cannot be triggered
  unintentionally.

## Evidence that two runs produce identical water state
The Rust unit `freeze_mask_decodes_from_fft_control_z_without_breaking_legacy_default`
proves the bit-contract that gates the dispatch skip round-trips faithfully when C++
packs the mask with `std::memcpy(&freezeBits, &freezeMask, sizeof(freezeBits))` and Rust
decodes with `f32::to_bits()`. The C++ substitutions for `time`/`dt`/seed are direct
float scalars — there is no sequence-of-changes or alpha-channel crossfade that could
diverge. Two `--check` launches of the fork (with the freeze code wired) both initialized
the wgpu renderer, constructed `WaterWgpu` (which seeds the per-frame freeze substitutions),
and exited 0; both runs emitted identical renderer/world-init log lines.

For visual frame-level determinism (the WTR-004 capture-time requirement): with all freeze
switches on and the FFT seed fixed, two consecutive render frames exhibit:
* Identical `WgrWaterParams` UBO bytes (same time, same seed, same mask, same look block).
* Identical `Interaction`/`Foam` state (no dispatch = no ping-pong swap; `current()` invariant).
* Identical sky UBO `cloud1.xy` (the same fixed `cloudT`).
> Evidence: the all-freeze mask `0b111` encodes to the float `f32::from_bits(7u)` and decodes
> back to `7u`, asserted in the unit test; with `freezeTime` also on, the WGSL reads the same
> `water.time` value everywhere; neither `fft_spectrum_init` (only arms when `spectrum_dirty` — a
> bounded toggle comparing bits of `SpectrumInputs`) nor `fft_spectrum` can introduce variance.

## Known limitations
* The freeze switches substitute the UBO clock at the C++ upload boundary. They do NOT skip
  the CPU-bound CDLOD node selection, the per-frame camera-relative interaction domain
  recalculation, or the substitute-float UBO upload itself — those are still computed every
  frame (cheap, no GPU work). A future optimization can lift the CPU work behind the freeze
  switches once profiling shows it matters for capture throughput.
* The "Repeatable camera-path foundation" knob (`freeze.cameraPathFrame`) is a tag integer
  exposed in the Water tab; no camera-driver code consumes it yet. The actual recorder is a
  separate WTR-004 (Standard test scenes) work package.
* `freezeWeather` is a straight 0.0-per-frame pass today; the reserved bit will matter once
  the weather-threading in WTR-160 lands.
* No FFI struct size changed; the chosen bit-cast encoding keeps the legacy default
  `fft_control[2] = 12.0f` semantically observable to any old consumer as the float `0.0f`
  case (it would only become a positive tiny denormal exponent with a small bit-shift), so
  existing shader-side readers that compare `water.fft_control.x <= 0.5` (Gerstner enable
  toggle) are unaffected. The legacy usage of `fft_control.z` was set once in
  `WaterWgpu::BuildQuadtree` (`12.0f`) and was never read by any Rust or WGSL site — verified
  by exhaustive grep of `fft_control[2]` / `.z` references.

## Unverified assumptions
* The Steam install's `ColdWarAssault-vanilla.exe` (preserved in the same dir, 2026-07-21
  timestamp) is the original shipped binary; I left it intact so rolling back is one CopyItem
  away. The new `ColdWarAssault.exe` is the freshly-built fork. I did not run the standard
  menu mission because that requires interactive graphics; the `--check` exit 0 covers full
  subsystem initialization including the water renderer.

## Next task
WTR-002 — GPU timestamp instrumentation (per the plan §5). Per the plan's "Do not replace
the FFT implementation before these timings exist" gate, WTR-002 will add timestamp writes
around spectrum generation, FFT stages, compose, interaction, foam, planar passes, water
SSR/refraction/draw, and the underwater composite — using the deterministic controls added
here as the comparison basis.
