# Water Spectral Core

## Scope

`rust/src/water/fft_spectrum_init.wgsl` initializes the persistent, four-layer 256x256 `h0` field. It uses only the existing `WgrWaterParams`: `fft_wind_sea` direction, speed, and sea state; `fft_control.y` seed; cascade lengths; and `wave_amp`. No C++ ABI, defaults, resource, Unity, or material path is changed.

The evolution pass remains Tessendorf's `h0(k)e^(iwt) + conjugate(h0(-k))e^(-iwt)`, so its real/Hermitian result and the complementary cascade partition are retained. Horizontal displacement remains `-i h k / |k|`; the existing evolution code has not gained a second `1 / |k|` factor.

## Formula Pass

The wind lobe is deep-water JONSWAP in k-space: the PM/JONSWAP `omega^-5 exp(-1.25(omega_p/omega)^4) gamma^r` form is converted using `omega = sqrt(gk)`, yielding the existing-compatible `k^-4 exp(-1.25 k_p^2/k^2)` radial form. It uses the JONSWAP sigma values 0.07/0.09 and a sea-state-mapped alpha and gamma. Frequency-dependent Cosine-2s directional spreading uses the reference's spread-power and normalization approximation.

A deterministic seed-derived cross-wind swell uses a low-energy, separately directed JONSWAP lobe. Its `k_p^4` compensation prevents its much longer wavelength from increasing the default visual height. The 0.72 conservative energy scale also keeps the upgraded default below the prior broad Phillips-like initialization while concentrating visible energy around the wind peak.

Finite-depth dispersion and the TMA correction are intentionally not implemented. CWR has no spectrum-depth/bathymetry input for this compute pass, so applying an arbitrary depth would be physically misleading.

## Reference And Attribution

Studied reference: `GarrettGunnell/Water`, commit `1673a12`,
`Assets/Shaders/FFTWater.compute` lines 44-130 and 133-226, retrieved from
`https://github.com/GarrettGunnell/Water` (MIT, Copyright (c) 2023 Garrett Gunnell).

This is an independent WGSL formula-level implementation, not a port of Unity code, resources, or shader source. The reference informed the JONSWAP peak factor, Cosine-2s spreading, and the decision to omit TMA without depth input. The upstream project also attributes its FFT/JONSWAP implementation to `gasgiant/FFT-Ocean`.
