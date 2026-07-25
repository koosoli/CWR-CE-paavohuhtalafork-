# Fix: Sky `sunHalo` Model Dereference Crash (`0xC0000005`)

## Problem Statement

When loading a landscape or mission configuration that omits the optional `sunHalo` sky model slot (or where `sunHaloName` fails to load), `loadSkyShape(sunHaloName, "sunHalo")` returns `nullptr`. 

Previously, `Landscape::Init()` contained a nested `if (haloShape)` block enclosing the initialization of `_sunObject` and `_moonObject`. If `haloShape` was `nullptr`, `_sunObject` and `_moonObject` were left uninitialized as `nullptr`. Subsequently, when `Landscape::DrawSky()` executed, it attempted to dereference `_sunObject` without a null guard, causing an immediate unhandled memory access exception `0xC0000005 at Poseidon::Landscape::DrawSky+0x78`.

## Resolution

1. **Decoupled Sun & Moon Shape Initialization**:
   - In `engine/Poseidon/World/Terrain/Landscape.cpp`, `_sunObject` and `_moonObject` shape loading has been decoupled from `haloShape`.
   - If `haloShape` is missing, `sunShape` and `moonShape` are initialized independently as standalone `ObjectPlain` shapes without halo geometry.

2. **Defensive Pointer Validation**:
   - In `engine/Poseidon/World/Terrain/LandscapeRender.cpp`, added explicit null guards around `_skyObject`, `_starsObject`, `_sunObject`, `_moonObject`, and their underlying `Shape` references inside `Landscape::DrawSky()`.

## Files Modified

- `engine/Poseidon/World/Terrain/Landscape.cpp`
- `engine/Poseidon/World/Terrain/LandscapeRender.cpp`

## Verification

- Tested against custom/unconfigured terrain configs lacking `sunHalo`.
- Compiled `PoseidonGame` target cleanly; engine starts without `0xC0000005` crash.
