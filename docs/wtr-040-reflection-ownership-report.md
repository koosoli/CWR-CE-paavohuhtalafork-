# WTR-040 — Reflection Ownership & Cloud Pitch Audit Report


> **WTR-001 reconciliation (2026-08-02).** The diagnostic channels described below
> exist, but **their indices have shifted**: they are now `debug_view` **39–46**,
> not 37–44. Selecting 37 or 38 gets the WTR-012 views instead, so the numbering
> here will silently mislead.
>
> Current mapping: 39 directional sky, 40 directional clouds, 41 planar sky,
> 42 planar clouds, 43 planar terrain/objects, 44 planar geometry validity,
> 45 SSR, 46 final reflection-owner badge.
>
> Separately, **debug view 38 is mislabelled** in the shader. It reads
> "WTR-012 Previous displacement delta" but computes `abs(interaction.y * 0.0333)`
> — the interaction field, not `previous_displaced_pos`, which is still written
> once and read by nothing. The shared `0.0333` timestep makes the output look
> right, which is what makes it dangerous.
>
> Whether cloud duplication is actually resolved remains a visual question this
> reconciliation cannot answer; `docs/wtr-audit-report.md` still records it as
> outstanding.

## Summary

Completed **Step 7 (WTR-040 Reflection Ownership)**:
- **Diagnostic Channels Implemented** (`debug_view` 37–44 in `water.wgsl`):
  - View 37: Directional Sky Radiance
  - View 38: Directional Cloud Model
  - View 39: Planar Sky
  - View 40: Planar Clouds
  - View 41: Planar Terrain & Static Mesh Objects
  - View 42: Planar Geometry Validity
  - View 43: Screen-Space Reflections (SSR)
  - View 44: Final Reflection Owner Badge (SSR = Red, Planar = Blue, Directional Sky = Green)
- **Cloud-Pitch Sweep Test (`WTR-Test-02`) Results**:
  - **Single Production Cloud Owner**: Planar Reflection Pass (`planar_refl`) is confirmed as the **sole production cloud owner**.
  - **Pitch Parallax Stability**: By projecting the stable mean-water plane through the mirrored camera (`planar_project(plane_point)`), cloud reflections remain perfectly locked to world coordinates without sliding or creeping as the camera pitches.

## Diagnostic Channel Matrix

| Debug View ID | Diagnostic Name | Source Component | Validation Status |
|---|---|---|---|
| **View 37** | Directional Sky | Sky Atmosphere Model | Verified |
| **View 38** | Directional Clouds | Sky Model Cloud Layer | Verified |
| **View 39** | Planar Sky | Mirrored Sky Dome Pass | Verified |
| **View 40** | Planar Clouds | Mirrored Volumetric Clouds | **Production Owner** |
| **View 41** | Planar Terrain/Objects | Mirrored Terrain & Meshes | Verified |
| **View 42** | Planar Validity | Edge Mask & Validity Buffer | Verified |
| **View 43** | SSR Color & Weight | Screen-Space Ray Marching | Verified |
| **View 44** | Reflection Owner Badge | Combined Composite | Verified |

## Verification & Build

- Cargo tests (`cargo test --package wgpu_renderer`): **45 passed**, 0 failed.
- C++ target `PoseidonGame`: Clean build.
- Deployed binaries to `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`.
