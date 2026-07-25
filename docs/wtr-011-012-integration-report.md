# WTR-011 / WTR-012 — Surface State Evaluation & Production Integration

## Summary

Completed **WTR-011 & WTR-012 Integration** in `water.wgsl`:
- **Authoritative Evaluation Function**: Implemented `evaluate_water_surface(in: VsOut) -> WaterSurfaceState` as the single authoritative surface evaluation entry point.
- **Production Velocity Consumer**: Surface velocity `state.velocity.xz` is consumed directly in production shoreline foam pattern generation (`foam_noise(in.base_xz + surface_vel * time, time)`).
- **New Debug Views**:
  - `35: Surface velocity (xz)` — False-color visualization of 2D surface movement vector speed.
  - `36: Previous displacement delta` — Diagnostic visualization of frame-to-frame displacement delta.

## Files Modified

| File | Change |
|---|---|
| `engine/WgpuRenderer/rust/src/water/water.wgsl` | Added `evaluate_water_surface()`, wired velocity into `foam_noise` advection, added debug views 35 & 36. |
| `docs/wtr-011-012-integration-report.md` | Created technical documentation report for WTR-011/012 integration. |

## Verification & Build

- `cargo test --package wgpu_renderer`: **45 passed**, 0 failed, 1 ignored.
- Deployed updated binaries to `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`.
