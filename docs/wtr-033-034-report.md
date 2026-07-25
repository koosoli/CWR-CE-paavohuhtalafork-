# WTR-033 / WTR-034 — Slope-Variance Roughness Compensation & CDLOD Displacement Bounds

## Summary

Implemented **WTR-033** (Slope-variance roughness compensation) and **WTR-034** (CDLOD displacement bounds) in the ocean pipeline.

## Implementation Details

1. **WTR-033 (Slope-Variance Roughness Compensation)**:
   - Updated `water_roughness` in `water.wgsl` to convert unresolved high-frequency slope variance from distant/attenuated cascades into microfacet roughness (`distance_roughness_compensation`).
   - Prevents the distant ocean from becoming an artificially flat mirror when fine wave detail fades out.

2. **WTR-034 (CDLOD Displacement Bounds)**:
   - Added conservative wave crest/trough and interaction impulse padding (`crestPadding = max(wave_amp * 4.0m, 4.0m)`) to CDLOD leaf node bounding calculation in `WaterWgpu.cpp`.
   - Prevents frustum and LOD selection from prematurely culling active wave crests or high-energy interaction ripples.

## Files Changed

| File | Change |
|---|---|
| `engine/WgpuRenderer/rust/src/water/water.wgsl` | Added slope variance roughness compensation to `water_roughness`. |
| `engine/WgpuRenderer/WaterWgpu.cpp` | Added `crestPadding` to `leafBounds` in CDLOD tree generation. |
| `docs/wtr-033-034-report.md` | Created documentation report for Phase WTR-030 completion. |

## Verification & Build

- `cargo test --package wgpu_renderer --profile rwdi`: **45 passed**, 0 failed, 1 ignored.
- Built and deployed updated executable and DLL to `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`.

## Next Phase

**Phase WTR-050 — Refraction, transparency and water optics** (**WTR-051 — Shared optical parameter block**).
