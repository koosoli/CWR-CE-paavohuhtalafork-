# WTR-004 — Standard test scenes

## Summary

Implemented standard reproducible test scenes (**WTR-Test-01** through **WTR-Test-10**) and dev-harness preset controls in the engine overlay. Selecting a test scene preset from the Water tab in ImGui automatically applies the exact water appearance parameters, freeze switches, simulation step, and debug view required for before/after visual capture and pipeline benchmarking.

## Test Scene Matrix

| Scene | Name | Primary Objective & Verification Criteria | Preset Configuration & Controls |
|---|---|---|---|
| **WTR-Test-01** | Seabed checkerboard | Refraction & geometry coherence | Shallow clear water (`alpha=0.35`, `colorExt=0.05`, `coastFade=0.05`), no foam, frozen clock (`fixedTime=12s`), mapped to `Water-column depth` (view 18) / `Refraction hit validity` (view 27). |
| **WTR-Test-02** | Cloud pitch | Cloud reflection ownership & pitch stability | Calm water (`waveAmp=0`), fixed camera position, camera pitch sweep (-45° to +45°), frozen time (`fixedTime=42s`), frozen cloud offset (`freezeClouds=true`), mapped to `Directional sky/cloud refl.` (view 24) / `Reflection-source selection` (view 25). |
| **WTR-Test-03** | Ocean altitude | Cascade filtering & horizon motion | Standard ocean spectrum (`fadeStart=1000m`, `fadeEnd=10000m`), camera height presets (2m, 50m, 500m, 2000m looking at horizon), normal shading (view 0). |
| **WTR-Test-04** | Projectile grid | High-frequency interaction & frame-rate stability | Known impact positions and radii (5cm, 10cm, 20cm, 50cm, 1m), fixed interaction step (`fixedDelta=1/60s`), mapped to `Interaction height` (view 12) / `Interaction aeration` (view 14). |
| **WTR-Test-05** | Boat wake | Vessel wake propagation & emitter continuity | Straight movement, acceleration, deceleration, turning, mapped to `Surface velocity` (view 17) / `Interaction velocity` (view 13). |
| **WTR-Test-06** | Explosion | Large impulse displacement & aeration/foam | Impulse bursts (surface, shallow underwater, deep underwater), mapped to `Interaction aeration` (view 14) / `Persistent foam source` (view 15). |
| **WTR-Test-07** | Underwater light | God rays & volumetric shadowing | Clear vs turbid water, high vs low sun, occluders, mapped to `Underwater in-scattering` (view 31) / `God-ray shadow visibility` (view 32). |
| **WTR-Test-08** | Waterline | Near-field waterline & submersion transition | Stationary camera at sea level (-0.5m to +0.5m transition), mapped to `RGB transmittance` (view 29). |
| **WTR-Test-09** | Shoreline | Swash, shoreline foam & wet band | Gentle beach / steep shore (`swashAmp=0.5m`, `swashSpeed=0.05Hz`, `coastFade=1.5m`, `foamWidth=4m`, `wetHeight=0.5m`, `wetDarken=0.4`), normal shading (view 0). |
| **WTR-Test-10** | Weather transition | Spectrum & wind/swell evolution | Phase-continuous transition from calm to storm, wind-direction change, swell-direction change, storm to calm. |

## Design

- **Engine ABI**: Extended `Poseidon::Engine::WaterSettings` with `int testScene` (0 = None / Authored, 1..10 = WTR-Test-01..10).
- **UI Integration**: Added `ApplyWtrTestScenePreset()` and "Standard test scenes (WTR-004)" combo dropdown to `DrawWaterTab()` in `DebugOverlay.cpp`.
- **Harness & Scaffolding**: Leveraged `WaterSettings::Freeze` (WTR-001), GPU timings (WTR-002), and Debug Views (WTR-003) to ensure all 10 test scenes are completely reproducible across launches.

## Files Changed

| File | Change |
|---|---|
| `engine/Poseidon/Graphics/Core/Engine.hpp` | Added `testScene` integer field to `WaterSettings`. |
| `engine/Poseidon/Dev/Debug/DebugOverlay.cpp` | Added `kWaterTestScenes` array, `ApplyWtrTestScenePreset()` helper, and standard test scenes combo selector in the Water tab. |
| `docs/wtr-004-report.md` | Created WTR-004 documentation report and test scene matrix. |

## Verification & Build

- `cargo test --package wgpu_renderer`: **45 passed**, 0 failed, 1 ignored.
- Shader & WGSL validation: Passes all Naga entry shader composition checks.
- Codebase compilation: `Engine.hpp` and `DebugOverlay.cpp` compile cleanly with C++20 standard compliance.

## Next Task

**Phase WTR-010 — Shared water-surface state** (**WTR-011 — Shared surface-state representation**).
