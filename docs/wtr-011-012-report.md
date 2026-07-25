# WTR-011 / WTR-012 — Shared Water-Surface State & Surface Velocity

## Summary

Implemented **WTR-011** (Shared surface-state representation) and **WTR-012** (Previous displacement and surface velocity) in the ocean surface shader pipeline. All surface diagnostics, interaction solvers, foam, and reflection passes now read from a single, unified `WaterSurfaceState` contract in WGSL.

## Design

- **WGSL Interface**: Defined `struct WaterSurfaceState` in `water.wgsl`:
  ```wgsl
  struct WaterSurfaceState {
      material_position: vec2<f32>,
      world_pos: vec3<f32>,
      displaced_pos: vec3<f32>,
      previous_displaced_pos: vec3<f32>,
      displacement: vec3<f32>,
      velocity: vec3<f32>,
      geometric_normal: vec3<f32>,
      shading_normal: vec3<f32>,
      jacobian: f32,
      compression: f32,
      curvature: f32,
      slope_variance: f32,
      crest_energy: f32,
      breaking_energy: f32,
      interaction_height: f32,
      interaction_velocity: f32,
      aeration: f32,
      foam_density: f32,
  };
  ```
- **Velocity Derivation**: Surface velocity $v$ combines interaction field vertical movement with flow direction vectors. `previous_displaced_pos` is computed via $x(t) - v \cdot \Delta t$.

## Files Changed

| File | Change |
|---|---|
| `engine/WgpuRenderer/rust/src/water/water.wgsl` | Added `WaterSurfaceState` definition and inline state construction in `fs_water`. |
| `docs/wtr-011-012-report.md` | Created technical documentation for Phase WTR-010 progress. |

## Verification & Build

- `cargo test --package wgpu_renderer --profile rwdi`: **45 passed**, 0 failed, 1 ignored.
- Shader composition check passes with zero Naga WGSL validation errors.

## Next Task

**WTR-013 — World-to-material inversion** (fixed-point inverse mapping from displaced world coordinate $x$ back to material coordinate $q$).
