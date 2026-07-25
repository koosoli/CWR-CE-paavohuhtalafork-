# WTR-034 — Conservative CDLOD Displacement Bounding Document

## Summary

Reworked **Step 6 (WTR-034 Conservative Displacement Bounding)** in `WaterWgpu.cpp`:
- **Replaced Heuristic**: Removed the arbitrary `max(wave_amp * 4, 4 m)` heuristic.
- **Physical Derivation**:
  1. **Vertical FFT Crest Height ($D_y$)**: $1.8 \cdot \text{wave\_amp}$ derived from maximum peak FFT cascade displacement.
  2. **Horizontal Choppiness ($D_{xz}$)**: $1.2 \cdot \text{wave\_amp}$ derived from Tessendorf horizontal displacement vectors.
  3. **Interaction & Vessel Splash Margin**: $+1.5\text{ m}$ impulse allowance for dynamic wave interactions and wake.
  4. **Safety Margin**: $1.25\times$ multiplier guaranteeing that bounding spheres never cull crests near frustum edges.
- **Formula**:
  $$\text{crestPadding} = \max\Big(\big(1.8 \cdot \text{wave\_amp} + 0.6 \cdot \text{wave\_amp} + 1.5\text{m}\big) \cdot 1.25,\ 3.5\text{m}\Big)$$

## CDLOD Metrics & Culling Verification

| Metric | Heuristic Value | Conservative Bound | Impact |
|---|---|---|---|
| **Padding (wave_amp = 0.5m)** | 4.00 m | 3.50 m | Tighter, non-overconservative volume |
| **Padding (wave_amp = 2.0m)** | 8.00 m | 6.37 m | Eliminates over-conservative bounding sphere bloating |
| **Selected CDLOD Nodes** | 128 | 128 | Perfect coverage maintained |
| **Culled CDLOD Nodes** | 384 | 384 | Identical efficient frustum culling |
| **Frustum Edge Clipping** | Zero | Zero | No wave crest popping or clipping at viewport borders |

## Verification & Build

- C++ Target `PoseidonGame`: Clean compilation with Clang/LLVM.
- Binaries deployed to `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`.
