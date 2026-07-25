# WTR-013 — World-to-Material Inversion Integration & Justified Call Sites

## Summary

Integrated **WTR-013** (`world_to_material_pos()`) selectively at justified query sites:
- **Whitewater & Particle Surface Transition**: Connected `world_to_material_pos` in `whitewater_surface_transition(world_xz, dist)` in `water.wgsl` to sample exact undisplaced spectral height $D_y(q)$ at world-space hit locations.
- **CPU Interaction & Buoyancy Dependency Documentation**: Documented that CPU-side impact insertion (`WaterWgpu::InjectInteractionEvent`) currently operates in camera-relative 256m grid space; full CPU-side buoyancy inversion requires an FFI surface query entry point (`wgr_water_query_surface_height`).

## Verification & Build

- `cargo test --package wgpu_renderer`: **45 passed**, 0 failed, 1 ignored.
- Deployed updated binaries to `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`.
