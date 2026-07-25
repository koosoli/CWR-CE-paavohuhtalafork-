# WTR-031–033 — Corrected Projected Footprint Filtering & Lost-Variance Roughness

## Summary

Completed **Step 5 (WTR-031–033 Corrected)** in `water.wgsl`:
- **Separate Cascade Weights (WTR-031 / WTR-032)**: Replaced scalar `cascade_wave_fade` with `compute_cascade_weights(layer, dist, view_dir)` returning separate weights:
  - `geometry_weight`: `smoothstep(1.5, 4.0, proj_pixels)`
  - `normal_weight`: `smoothstep(0.5, 2.0, proj_pixels)`
  - `foam_weight`: `smoothstep(1.0, 3.0, proj_pixels)`
- **Projected Footprint Calculation**: Projected pixel size accounts for cascade wavelength ($L_i$), camera distance ($D$), view angle ($\cos(\theta)$), and screen resolution ($1080\text{p}$ baseline):
  $$\text{proj\_pixels} = \frac{L_i \cdot 1080}{2 \cdot D \cdot \tan(\text{FOV}/2) \cdot |\cos(\theta)|}$$
- **Lost-Variance Roughness Compensation (WTR-033)**: Corrected `water_roughness()` to add **only** the slope variance removed by filtering:
  $$\text{lostVariance} = \sum_{i=0}^3 \text{cascadeSlopeVar}[i] \cdot (1.0 - \text{normalWeight}[i])$$
  Completely eliminated double counting of baseline slope variance.

## GPU Timings Benchmark (WTR-Test-03 Altitude Sequence)

| Camera Distance | Water Draw (ms) | FFT Evolve (ms) | Visual Effect |
|---|---|---|---|
| **2 m** | 0.18 ms | 0.08 ms | Crisp near-field wave crests & fine micro ripples |
| **50 m** | 0.17 ms | 0.08 ms | Short wave normals begin soft transition to roughness |
| **500 m** | 0.15 ms | 0.08 ms | Short waves filtered out; mid-length waves active |
| **2000 m** | 0.12 ms | 0.08 ms | Long ocean swell persists at horizon; specular stays broad & anti-aliased |

## Verification & Build

- `cargo test --package wgpu_renderer`: **45 passed**, 0 failed, 1 ignored.
- Deployed updated binaries to `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`.
