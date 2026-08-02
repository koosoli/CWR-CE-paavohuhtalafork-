# Preview-0 Tier-1 validation contract

This is the bounded `PERF-001` configuration for Preview 0. It deliberately
defines one reproducible Windows/WGPU path, rather than a hardware matrix.

## Configuration

- Support level: `TIER_1_RELEASE_BLOCKING`
- OS: Windows 11 Home x64, build 26200
- CPU: Intel Core i7-11700F (8 cores / 16 logical processors)
- System RAM: 16 GB
- GPU: NVIDIA GeForce RTX 3070 (8 GB), driver `32.0.15.9579`
- WGPU backend: Vulkan
- Resolution and scaling: 3441 x 1440 native, VSync on, no dynamic resolution
- Target: 30 FPS / 33.33 ms frame time. This is the Preview-0 release gate,
  not a mandate to change WGPU's visual identity to match GL33.
- CPU-frame budget: 33.33 ms in the original-training reference capture.
- GPU-frame budget: 33.33 ms in the original-training reference capture.
- Dedicated-server tick rate and tick budget: not a Preview-0 gate; server
  validation is deferred to `TEST-004`.
- Expected player/entity scale: original single-player training mission; this
  is deliberately a renderer-validation configuration, not a multiplayer load
  target.
- Network bandwidth target: not applicable to this local Tier-1 renderer gate.
- Maximum persistent derived-cache size: 2 GB advisory cap; original game data
  remains read-only and removable derived caches must preserve the fallback.
- Release status: blocking for this one Windows/WGPU configuration only.
- CMake preset/build tree: `build/win-x64-clang-dbg` for the pinned evidence;
  `build/win-x64-clang-rel` is also accepted for local smoke deployment.
- Build target: `PoseidonGame`
- Backend: `--render wgpu`
- Windowed developer launch: `--window --dev`
- Installed smoke-test location: `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe`
- Required adjacent runtime: matching `wgpu_renderer.dll`

## Gates

1. `cmake --build build/win-x64-clang-dbg --target PoseidonGame --parallel 4` exits successfully.
2. `cargo test --package wgpu_renderer` exits successfully.
3. The executable and matching renderer DLL are deployed together to the installed game directory.
4. `ColdWarAssault.exe --render wgpu --window --dev` starts and its log proves WGPU was selected; GL33 fallback is a failure.
5. The Water tab exposes non-zero GPU timing data after a water-visible frame.
6. Capture the executable hash, DLL hash, Git revision, adapter/backend, driver, timestamp availability, and the launch log for the result bundle.
7. `scripts/run_preview0_wgpu_check.ps1 -LifecycleSmoke` performs a real
   window resize plus three Windows minimise/restore transitions in the normal
   game loop, records each SDL lifecycle event, and saves a post-restore frame
   capture.
8. `scripts/run_preview0_wgpu_check.ps1 -ProfileOverlaySmoke` captures the
   live Profile tab, including the selected WGPU renderer and negotiated
   capability flags.
9. `scripts/run_preview0_wgpu_check.ps1 -MissionCaptureSmoke` captures the
   original training mission after gameplay begins, and requires non-zero WGPU
   GPU timings plus populated near-grass instances.

## Preview-0 renderer budget shares

These shares are review budgets, not automatic quality-reduction triggers. A
measurement above a share requires a comparable capture and a correctness
review; it must not silently disable water, reflections, terrain, or lighting.

| Renderer feature | Tier-1 share | Evidence and interpretation |
| --- | ---: | --- |
| Water simulation, planar reflection, and draw | 16.0 ms | The clean original-training capture measured 2.53 ms for the water group; an interactive water-dominant view measured 15.52 ms. The latter is an allowed camera-dependent upper envelope, not a reason to reduce visual quality. |
| Grass placement, prepass, colour, and shadow | 6.0 ms | Clean original-training capture: 2.20 ms with populated near and mid grass. |
| Terrain, objects, sky, post-processing, UI, and remaining GPU work | 11.0 ms | Remainder of the 33.33 ms GPU budget after the water and grass allocations. |
| Explicit unallocated margin | 0.33 ms | Retained for timestamp quantisation and small measurement variation. |

The clean original-training reference capture measured 16.33 ms GPU total.
The budget therefore records a release guardrail and headroom, not a claim that
every camera angle has identical frame time.

## Release boundary

This gate proves one Tier-1 path only. Cross-platform compatibility, alternate adapters, and exhaustive scene coverage remain later work.
