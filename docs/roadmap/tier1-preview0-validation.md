# Preview-0 Tier-1 validation contract

This is the bounded `PERF-001` configuration for Preview 0. It deliberately
defines one reproducible Windows/WGPU path, rather than a hardware matrix.

## Configuration

- OS: Windows x64
- CMake preset/build tree: `build/win-x64-clang-dbg`
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

## Release boundary

This gate proves one Tier-1 path only. Cross-platform compatibility, alternate adapters, and exhaustive scene coverage remain later work.
