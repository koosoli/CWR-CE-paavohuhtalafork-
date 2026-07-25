# WTR-036A — GodotOceanWaves vs CWR-CE Water System Reference Parity Audit

## Executive Summary
This document provides a field-by-field architectural and source-level comparison between `krautdev/GodotOceanWaves` (located at `inspiration/GodotOceanWaves-main`) and the current CWR-CE WebGPU Water System.

Our target is to match or exceed GodotOceanWaves close/medium-range wave visual fidelity while providing superior large-world aerial anti-repetition across Arma Cold War Assault's multi-kilometer terrain scales.

---

## Detailed Field-by-Field Comparison Matrix

| Feature / Field | GodotOceanWaves Reference | CWR-CE Current Implementation | Classification | Notes / Technical Gap |
| :--- | :--- | :--- | :--- | :--- |
| **Cascade Count** | 3 cascades (88m, 57m, 16m tile lengths) | 4 cascades (48m, 144m, 432m, 1296m) | `different by design` | Reference uses 3 cascades for demo; CWR-CE uses 4 cascades for large-scale flight distances up to 10+ km. |
| **Texture Resolution** | 256x256 (or 512x512) RGBA16F FFT textures per cascade | 256x256 RGBA16F FFT textures across 4 cascades | `equivalent` | Both compute 256² spatial FFT grids on GPU. |
| **Tile Lengths** | Non-harmonic: 88m, 57m, 16m | Harmonic: 48m, 144m, 432m, 1296m (exact ×3) | `partial` | Our current ×3 harmonic ratios cause repeat pattern at 1,296m. Must adopt non-harmonic coprime lengths in WTR-037. |
| **Common Repetition Distance** | ~5,016m (lcm of 88, 57) for geometry cascades | 1,296m (exact harmonic period) | `missing` (WTR-037 target) | CWR-CE currently repeats every 1,296m. Needs coprime lengths extending repetition beyond >50 km. |
| **Per-Cascade Seeds** | Independent per-cascade 2D integer seed (`ivec2 seed`) | Global seed with per-layer offset (`seed + layer`) | `partial` | Reference allows fully independent per-cascade spectrum seeds. |
| **Per-Cascade Time/Phase** | Shared time variable `time` across cascades | Shared time `time` across cascades | `equivalent` | Both evaluate continuous phase $\omega(k) \cdot t$. |
| **Wind Speed** | Independent per cascade (e.g., C0: 10m/s, C1: 5m/s, C2: 20m/s) | Shared global wind vector (`fft_wind_sea.z`) | `missing` (WTR-036B target) | Reference allows independent wind velocity per cascade layer. |
| **Wind Direction** | Independent per cascade (e.g., C0: 20°, C1: 15°, C2: 20°) | Shared global wind direction (`fft_wind_sea.xy`) | `missing` (WTR-036B target) | Reference allows independent wind heading per cascade. |
| **Fetch** | Independent per cascade (e.g., C0: 150km, C1: 150km, C2: 550km) | Shared global fetch value | `missing` (WTR-036B target) | Reference tunes peak frequency $\omega_p$ per cascade via fetch. |
| **Swell** | Independent per cascade (0.0 to 1.0) | Global seeded cross-swell | `partial` | Both support swell, but reference exposes per-cascade swell weight. |
| **Directional Spread** | Independent Hasselmann spreading per cascade | JONSWAP / Donelan-Banner spreading | `equivalent` | Both apply frequency-dependent directional spreading functions. |
| **Detail Suppression** | Independent high-frequency attenuation (`detail`) per cascade | Global band-pass cutoff filters | `partial` | Reference uses $e^{-(1-\text{detail})^2 k^2}$ gaussian cutoff per cascade. |
| **Displacement Scale** | Per-cascade multiplier (`disp_scale`: 1.0, 0.75, 0.0) | Global wave amplitude | `missing` (WTR-036B target) | Reference uses Cascade 2 purely as normal/foam detail with 0 displacement. |
| **Normal Scale** | Per-cascade multiplier (`normal_scale`: 1.0, 1.0, 0.25) | Calculated from displacement gradients | `missing` (WTR-036B target) | Reference allows fine-tuning normal strength per cascade. |
| **Foam Scale** | Per-cascade whitecap & foam amount parameters | Global foam generation and interaction aeration | `better` | CWR-CE includes physical WTR-080 foam advection & interaction aeration; needs per-cascade emission controls. |
| **Spectrum Model** | TMA / JONSWAP spectrum with depth attenuation | JONSWAP spectrum with band-pass partitioning | `equivalent` | Both utilize JONSWAP formulation. |
| **Finite-Depth Dispersion** | Kitaigorodskii depth attenuation $\tanh(kh)$ | Shallow water dispersion and seabed depth interaction | `better` | CWR-CE integrates real-time terrain depth map for shallow water refraction/swash. |
| **Filtering** | World-space pixels-per-meter (PPM) bicubic / bilinear normal blend | Slope variance addition + central difference normals | `partial` (WTR-038 target) | Reference uses bicubic B-spline filtering based on $PPM = \frac{\text{map\_size}}{\text{tile\_length}}$. |
| **Geometry LOD** | 8 km vertex clipmap mesh (`clipmap_high_8k.obj`) | CDLOD quadtree mesh with geomorphing | `better` | CWR-CE CDLOD quadtree provides seamless infinite horizon coverage without massive static OBJ meshes. |
| **Foam Generation** | Jacobian determinant thresholding $J < \text{whitecap}$ | WTR-081 breaking energy + WTR-082 divergence + WTR-085 advection | `better` | CWR-CE persistent foam advection field is physically advected across time. |
| **Spray** | Particle emitter at wave crests (`sea_spray_particle`) | WTR-090 unified GPU whitewater particles + `WaterSource` droplets | `better` | CWR-CE includes 3D droplet spray, surface foam clumps, and underwater bubbles. |
| **Depth Color** | 3-band depth absorption (`depth_color_consumption`) | Physical RGB Beer-Lambert wavelength extinction ($e^{-\mathbf{\sigma}_t d}$) | `better` | CWR-CE uses physical Beer-Lambert extinction with coastal turquoise to deep navy blue gradient. |
| **GPU Cost** | ~3 compute passes (IFFT) + 8 km clipmap mesh draw | ~4 compute passes + interaction solver + CDLOD draw | `equivalent` | Similar performance footprint (~0.8 - 1.2 ms on modern GPUs). |

---

## Technical Action Plan Based on Audit

1. **WTR-036B**: Upgrade Rust and C++ FFT configuration structures to support independent per-cascade tile lengths $(L_x, L_y)$, displacement scales, normal scales, wind speeds, wind directions, fetch, swell, spread, detail, and seeds.
2. **WTR-036C**: Add `GodotOceanWaves Reference-Style` preset (88m, 57m, 16m) alongside our production 4-cascade preset for live side-by-side A/B comparison.
3. **WTR-037**: Replace exact $\times 3$ harmonic tile lengths ($48\text{m}, 144\text{m}, 432\text{m}, 1296\text{m}$) with non-harmonic coprime lengths ($37.0\text{m}, 89.0\text{m}, 211.0\text{m}, 503.0\text{m}$) so that the mathematical common repeat is pushed beyond $>50\text{ km}$.
4. **WTR-038**: Implement PPM (Pixels-Per-Meter) bicubic normal filtering in `water.wgsl` to eliminate high-frequency normal aliasing/shimmering at medium/long distances.
