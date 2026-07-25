# WTR Water System Smoke Test Instructions

To run the interactive smoke test on a compiled build (`ColdWarAssault.exe`):

## Launch Commands (Direct PowerShell & Start-Process)

```powershell
# Direct inside the Steam game directory:
.\ColdWarAssault.exe --render wgpu --window --dev

# Or via Start-Process:
Start-Process -FilePath "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe" -ArgumentList "--render wgpu --window --dev"
```

## Visual Changes to Expect (Phase WTR-030 Completed)

1. **Horizon Swell & Altitude Waves (WTR-031 / WTR-032)**:
   - **Visual Difference**: Short ripples dissolve near the view, but **large ocean swell and wave silhouettes stay visible all the way to the horizon** and in high-altitude airplane views (no more flat mirror cutoff at `fade_end`).

2. **Distant Sun Glints & Specular Lobe (WTR-033)**:
   - **Visual Difference**: Faraway water retains a broad, natural sun-glint highlight without shimmering or aliasing, because unresolved wave slope variance is converted into microfacet roughness.

3. **Crest & Ripple Bounds (WTR-034)**:
   - **Visual Difference**: High wave peaks and explosion/projectile ripples remain stable at frustum edges without sudden popping or culling.

## Checklist for In-Game Verification

1. **Open Debug Overlay**: Press `Ctrl + \`` (or `~`) to open the engine overlay window.
2. **Open Water Tab**: Navigate to the **Water** tab.

3. **WTR-004 Standard Test Scenes**:
   - **WTR-Test-01 — Seabed checkerboard**: Select from combo. Verify clear shallow water, correct column-depth transparency, and refraction without screen-edge distortion.
   - **WTR-Test-02 — Cloud pitch**: Select from combo. Tilt camera from −45° to +45°. Confirm cloud reflections are pitch-stable without popping between planar and sky fallback.
   - **WTR-Test-03 — Ocean altitude**: Select from combo. Confirm long-distance swell and smooth ocean horizon at altitudes from 2m to 2000m.
   - **WTR-Test-04 — Projectile grid**: Select from combo. Confirm fine ripple propagation and fixed-step simulation rate.
   - **WTR-Test-09 — Shoreline**: Select from combo. Confirm swash oscillation, shoreline foam band, and damp sand terrain darkening.

4. **WTR-003 Debug Views (Diagnostic Overlay)**:
   - Use the **Debug view** combo or press `Ctrl + Shift + W` to cycle on-surface diagnostics.
   - Check key views:
     - `1: FFT displacement`
     - `12: Interaction height`
     - `18: Water-column depth`
     - `24: Directional sky/cloud reflection`
     - `25: Reflection-source selection` (Red = SSR, Blue = Planar, Green = Sky)
     - `27: Refraction hit validity`

5. **WTR-002 GPU Timestamps**:
   - Scroll to the bottom of the Water tab.
   - Confirm non-zero execution times (in milliseconds) for `SpectrumEvolve`, `FftHorizontal`, `FftVertical`, `FftCompose`, `Interaction`, `Foam`, and `WaterDraw`.
