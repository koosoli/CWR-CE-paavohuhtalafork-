# GodotOceanWaves Technical Reference & Attribution

## Overview

This repository adapts algorithms, shader lighting models, and wave spectrum formulations from **GodotOceanWaves** (KrautDev / GodotOceanWaves fork).

- **Original Repository**: [https://github.com/krautdev/GodotOceanWaves](https://github.com/krautdev/GodotOceanWaves)
- **License**: MIT License
- **Author**: KrautDev & Contributors

---

## MIT License Notice

```text
MIT License

Copyright (c) 2024 KrautDev

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## Adapted Components & Algorithms

1. **Texel-Marsen-Arsloe (TMA) Spectrum & Depth Attenuation**:
   - $\Phi(\omega)$ depth attenuation factor for shallow-water wave energy reduction.
   - Hasselmann directional spread function $D_{\text{mixed}}(\omega, \theta)$ mixing flat and directional spread.
2. **Subsurface Scattering (SSS) & Lighting**:
   - Translucent backlit crest light scattering with turquoise modifier (`vec3(0.9, 1.15, 0.85)`).
   - GGX specular BSDF with Smith masking-shadowing function.
3. **Normal Filtering & Anti-Aliasing**:
   - 3rd-order bicubic B-spline filtering (`texture_bicubic`) with pixels-per-meter (PPM) LOD blending.
4. **Godot Reference 3-Cascade Configuration**:
   - **Cascade A**: 88m tile, displacement=1.0, normal=1.0, wind=10m/s, dir=20°, fetch=150km, swell=0.8, spread=0.2.
   - **Cascade B**: 57m tile, displacement=0.75, normal=1.0, wind=5m/s, dir=15°, fetch=150km, spread=0.4.
   - **Cascade C**: 16m tile, displacement=0.0 (normal/foam only), normal=0.25, wind=20m/s, dir=20°, fetch=550km, spread=0.4.
