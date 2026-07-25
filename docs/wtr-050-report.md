# WTR-050 — Refraction, Transparency & Water Optics

## Summary

Implemented **Phase WTR-050** (Refraction, transparency and water optics) in `water.wgsl`:
- **WTR-051 (Shared optical parameter block)**: Integrated physical water index of refraction ($n = 1.333$) and wavelength-dependent RGB absorption coefficients ($\sigma_a = [0.35, 0.07, 0.02]$) where red light attenuates fastest underwater.
- **WTR-052 (Physical Fresnel foundation)**: Derived normal-incidence reflectance $F_0 = 0.02037$ directly from IOR ($1.333$), maintaining separate artistic reflection multiplier ($1.45$) and bias ($0.025$).
- **WTR-053 (Snell's Law Refraction)**: Calculated physical refracted view direction $R_{\text{refract}} = \text{refract}(V, N, 1 / 1.333)$ to distort seabed UVs dynamically based on wave slope and depth.
- **WTR-056 (Water Thickness & Path Length)**: Evaluated optical path length $L = \text{depth} / |\cos(\theta)|$ and applied RGB Beer-Lambert transmission $T = e^{-\sigma_a L}$.

## Visual Changes Expected

1. **Seabed Refraction Bending (WTR-053)**:
   - **Before**: Seabed distortion used an arbitrary static pixel normal offset.
   - **Now**: The submerged terrain and objects refract physically through passing wave slopes according to Snell's law ($n = 1.333$), bending visibly under moving wave crests.

2. **Depth Color Shift & Clear Shallows (WTR-051 / WTR-056)**:
   - **Before**: Water body tinting used a uniform single-color linear fade with depth.
   - **Now**: Shallows are crystal clear, while deeper water progressively shifts to deep turquoise/blue as red light is absorbed faster than blue/green light over longer optical path lengths ($L = \text{depth} / |\cos(\theta)|$).

## Verification & Build

- `cargo test --package wgpu_renderer`: **45 passed**, 0 failed, 1 ignored.
- Deployed updated binaries to `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`.
