# CWR-CE Water Rework (WTR) Comprehensive Audit Report

**Date**: July 25, 2026  
**Checkpoint Branch**: `checkpoint/wtr-audit-phase`  
**Checkpoint Commit**: `6171ff7`  

---


> **WTR-001 reconciliation (2026-08-02).** This audit was accurate when written on
> 2026-07-25 and is largely accurate still — its findings on `WaterSurfaceState`
> were independently re-derived a week later and matched. Two entries have since
> drifted and are superseded:
>
> - **WTR-032** (*"single fade per cascade used across both geometry and normals;
>   separate weights missing"*) — **now implemented.** `CascadeWeights` in
>   `water/water.wgsl` carries separate `geometry_weight`, `normal_weight` and
>   `foam_weight`, each from its own `smoothstep` on projected pixel footprint.
>   There is still no `roughness_weight`; roughness comes from `slope_variance`
>   via `water_roughness()`.
> - **WTR-013** (*"`world_to_material_pos()` implemented but has no callers"*) —
>   the function **is** called now (`water.wgsl:470`). Note the related
>   `WaterSurfaceState.material_position` field is still written and never read,
>   so the underlying observation survives one level down.
>
> Entries re-confirmed as still true: **WTR-011** and **WTR-012**. Six
> `WaterSurfaceState` fields are written and never read back —
> `material_position`, `displaced_pos`, `previous_displaced_pos`, `jacobian`,
> `interaction_height`, `breaking_energy` — so previous displacement still has no
> consumer, exactly as recorded here.
>
> Kept rather than rewritten, per RND-030: the reasoning is still worth reading.

## 1. Executive Summary & Revised Status Matrix

Following the AI Overseer's review directive, all water rework tasks have been audited against the actual production codebase. Several tasks previously marked as complete have been reclassified as **provisional**, **infrastructure-only**, or **partial** to reflect the gap between initial helper/shader implementation and full system integration.

### Corrected Status Matrix

| Task ID | Description | Status | Audit Notes |
|---|---|---|---|
| **WTR-001** | GPU CDLOD Surface Geometry | **Complete** | Quadtree grid, morphing, sea datum integration verified. |
| **WTR-002** | GPU Timestamps & Profiling | **Complete** | Non-zero GPU pass timings in debug overlay verified. |
| **WTR-003** | Diagnostic False-Color Debug Views | **Complete** | 36 false-color debug modes exposed via combo & shortcut. |
| **WTR-004** | Standard Test Scenes & Harness | **Partial** | Preset combo exists (`WTR-Test-01`..`10`), but deterministic camera paths, frame-stepping, and state snapshotting are unbuilt. |
| **WTR-011** | Shared `WaterSurfaceState` | **Verify / In Progress** | Struct defined in WGSL, but downstream passes (`foam`, `ssr`, `refraction`) still take individual parameters. |
| **WTR-012** | Previous Displacement & Velocity | **Infrastructure Only** | Velocity & previous displacement calculated inline in WGSL state, but currently unconsumed by foam/drag. |
| **WTR-013** | World-to-Material Inversion | **Infrastructure Only** | `world_to_material_pos()` solver implemented in `water.wgsl`, but currently has 0 active call sites. |
| **WTR-031** | Frequency-Aware Distance Filtering | **Provisional** | Wavelength-proportional fade implemented (`cascade_wave_fade`), but lacks footprint/FOV/angle scaling. |
| **WTR-032** | Separate Geometry & Shading Visibility | **Incomplete** | Single fade per cascade used across both geometry and normals; separate weights for foam/roughness missing. |
| **WTR-033** | Slope-Variance Roughness Compensation | **Provisional** | Roughness compensation currently double-counts total slope variance instead of strictly using lost variance $\sum \text{Var}_i (1 - w_i)$. |
| **WTR-034** | CDLOD Displacement Bounds | **Provisional** | Uses provisional padding `max(wave_amp * 4, 4m)`; requires exact spectrum displacement bounds & culling profiling. |
| **WTR-040** | Reflection Ownership & Cloud Stability | **Not Complete** | Composition blend exists, but cloud duplication remains, cloud-pitch A/B harness missing, and SSR confidence unbuilt. |
| **WTR-050** | Refraction, Optics & Beer-Lambert | **Paused** | Snell's law and RGB Beer-Lambert implemented locally; paused per Overseer directive pending WTR-004/011-040 completion. |

---

## 2. Detailed Task-by-Task Audit Findings

### WTR-004 — Standard Test Scenes & Harness
- **Current State**: UI combo box with 10 presets in `DebugOverlay.cpp`.
- **Missing Capabilities**:
  1. Deterministic camera animation & position paths.
  2. Snapshot and restoration of previous user water/debug settings upon exiting test scene.
  3. Frame-stepping controls (`Start`, `Stop`, `Restart`, `Step Frame`).
  4. One-shot event injection triggers (e.g. projectile impacts, boat wakes).
  5. Availability/disabled states for test presets targeting un-implemented features (e.g., caustics, god rays).
  6. Reproducible metadata log for regression testing across identical runs.
  7. Strict decoupling: camera/event orchestration currently lives inside debug UI instead of a dedicated test harness service.

### WTR-011, WTR-012, WTR-013 — Surface State & Inversion
- **`WaterSurfaceState` Struct**: Defined in [water.wgsl](file:///c:/Users/mail/OneDrive/Documents/GitHub/CWR-CE-paavohuhtalafork-/engine/WgpuRenderer/rust/src/water/water.wgsl#L585-L607). Produced inline in `fs_water` (line 690).
- **Consumption Gap**: Downstream helper functions (`foam_source`, `refraction`, `ssr`) do not yet take `state: WaterSurfaceState` directly.
- **Previous Displacement & Velocity (WTR-012)**: `previous_displaced_pos` is calculated as `in.world_pos - state.velocity * 0.0333`. It is an inline back-projection without persistent frame-history texture storage and is currently unused by foam advection.
- **World-to-Material Inversion (WTR-013)**: `world_to_material_pos(world_xz, dist)` is defined in `water.wgsl` (line 191), but has **0 call sites** in the active shading pipeline.

### WTR-031 & WTR-032 — Distance Filtering & Visibility Weights
- **Current Code**:
  ```wgsl
  fn cascade_wave_fade(layer: i32, dist: f32) -> f32 {
      let length_m = max(wp.fft_cascade_lengths[layer], 48.0);
      let ratio = length_m / 48.0;
      return 1.0 - smoothstep(wp.fade_start * ratio, wp.fade_end * ratio, dist);
  }
  ```
- **Audit Findings**:
  - `cascade_wave_fade` scales distance by wavelength ratio $\frac{L_i}{48}$, which is a strong improvement over a single global distance fade.
  - However, it does **not** evaluate projected pixel footprint (which depends on camera FOV, screen resolution, and view angle).
  - WTR-032 requires separate weight arrays: `geometryWeight[4]`, `normalWeight[4]`, `foamWeight[4]`, `roughnessContribution[4]`. Currently, a single `cascade_wave_fade` is applied uniformly to geometry and normal slope.
  - Missing A/B toggle between legacy single fade and wavelength-scaled fade.

### WTR-033 — Roughness Compensation & Slope-Variance Audit
- **Old Roughness Equation**:
  ```wgsl
  let legacy_floor = sqrt(2.0 / max(spec_power + 2.0, 2.0));
  let fft_slope = sqrt(clamp(fft_slope_variance, 0.0, 0.25));
  let micro_slope = length(shading_normal.xz - base_normal.xz);
  return clamp(legacy_floor + fft_slope * 0.26 + micro_slope * 0.35, 0.075, 0.32);
  ```
- **New Roughness Equation (Audited)**:
  ```wgsl
  let distance_roughness_compensation = 0.15 * (1.0 - cascade_wave_fade(0, dist)) * fft_slope;
  return clamp(legacy_floor + fft_slope * 0.26 + micro_slope * 0.35 + distance_roughness_compensation, 0.075, 0.45);
  ```
- **Flaw Identified**: `fft_slope` is included in the base roughness calculation, and then `distance_roughness_compensation` adds a fraction of `fft_slope` again. This **double-counts** total slope variance instead of strictly adding only the **lost variance** ($\sum \text{Var}_i \cdot (1 - w_i)$).

### WTR-034 — CDLOD Displacement Bounds & Culling
- **Current Bounds Code**:
  ```cpp
  const float crestPadding = std::max(_params.wave_amp * 4.0f, 4.0f);
  mn = std::min(mn, h - crestPadding);
  mx = std::max(mx, h + crestPadding);
  ```
- **Audit Findings**:
  - Hard-coded scalar $4.0\text{ m}$ is provisional.
  - Does not compute exact maximum horizontal ($D_{xz}$) or vertical ($D_y$) FFT spectrum displacement envelope.
  - Requires benchmark comparison measuring selected CDLOD node count, CPU/GPU draw time, and crest culling across calm ($0.5\text{m}$ wave) vs storm ($4.0\text{m}$ wave) states.

### WTR-040 — Reflection Ownership & Cloud Stability
- **Audit Findings**:
  - Confirming the existing blend order (`sky` $\rightarrow$ `ssr` $\rightarrow$ `planar`) did not fix the underlying dual-cloud ownership (clouds reflected via environment map AND planar reflected camera).
  - Explicit diagnostic channels (separate planar terrain vs planar sky, SSR confidence map, cloud-reflection owner toggle) are incomplete.
  - Pitch stability test harness (`WTR-Test-02`) requires camera pitch sweep without cloud distortion crawling.

---

## 3. List of Changed Files in Checkpoint (`6171ff7`)

- `engine/Poseidon/Dev/Debug/DebugOverlay.cpp`
- `engine/Poseidon/Graphics/Core/Engine.hpp`
- `engine/Poseidon/World/Terrain/Landscape.cpp` (Standalone `sunHalo` crash fix)
- `engine/Poseidon/World/Terrain/LandscapeRender.cpp` (Standalone `sunHalo` crash fix)
- `engine/WgpuRenderer/WaterWgpu.cpp`
- `engine/WgpuRenderer/rust/src/water/water.wgsl`
- `.agents/SMOKE_TEST_INSTRUCTIONS.md`
- `docs/wtr-004-report.md`
- `docs/wtr-011-012-report.md`
- `docs/wtr-013-report.md`
- `docs/wtr-033-034-report.md`
- `docs/wtr-050-report.md`

---

## 4. Recommended Action Plan & Next Steps

1. **Keep Checkpoint Branch Intact**: Retain `checkpoint/wtr-audit-phase` as the master reference.
2. **Finish WTR-004 Test Harness Foundation**: Build camera path recorder/player, frame-stepping, and state snapshot/restore.
3. **Refactor WTR-011 / 012 / 013**: Wire `WaterSurfaceState` through all downstream surface functions (`foam_source`, `refraction`, `ssr`) and connect `world_to_material_pos` to impact/buoyancy queries.
4. **Fix WTR-031–034**:
   - Add separate weights `geometryWeight[4]`, `normalWeight[4]`, `foamWeight[4]`, `roughnessContribution[4]`.
   - Calculate true lost-variance $\sum \text{Var}_i (1 - w_i)$ for roughness compensation (eliminate double counting).
   - Derive exact CDLOD displacement bounds from FFT spectrum peaks + choppy factor.
   - Add A/B toggle for global vs wavelength-scaled fade.
5. **Implement WTR-040 Reflection Ownership & Diagnostics**: Disambiguate cloud reflection ownership and expose reflection-source debug channels.
6. **Resume Phase WTR-050**: Only after WTR-004, 011–013, 031–034, and 040 pass acceptance criteria.
