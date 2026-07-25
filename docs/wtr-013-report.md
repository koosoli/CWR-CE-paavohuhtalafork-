# WTR-013 — World-to-Material Inversion

## Summary

Implemented **WTR-013** (World-to-material inversion) in `water.wgsl`. The function `world_to_material_pos(world_xz, dist)` resolves the undisplaced spectral material coordinate $q$ from a displaced world coordinate $x$ using 3 fixed-point iterations ($q_{k+1} = x - D_{xz}(q_k)$).

## Algorithm & Design

- **Fixed-Point Iteration**:
  ```text
  q0 = world_xz
  q1 = world_xz - horizontalDisplacement(q0)
  q2 = world_xz - horizontalDisplacement(q1)
  q3 = world_xz - horizontalDisplacement(q2)
  ```
- **Targeted Application**: Applied selectively for world-space event sampling, impact injection, buoyancy calculation, and particle surface transitions without adding unnecessary per-fragment overhead to standard shading paths.

## Files Changed

| File | Change |
|---|---|
| `engine/WgpuRenderer/rust/src/water/water.wgsl` | Added `world_to_material_pos()` fixed-point inversion solver. |
| `docs/wtr-013-report.md` | Created technical documentation report for WTR-013. |

## Verification & Build

- `cargo test --package wgpu_renderer --profile rwdi`: **45 passed**, 0 failed, 1 ignored.
- All Naga entry point shader composition and WGSL syntax checks pass.

## Next Phase

**Phase WTR-020 — Spectrum quality and FFT state** (**WTR-021 — Separate initial spectrum from time evolution**).
