# World and rendering ownership map

This is the current production-path inventory for CORE-000. It identifies the
owner of each system, its principal consumers, and the boundary that future
modernisation work must preserve.

| System | Primary owner and production path | Boundary / consumers |
| --- | --- | --- |
| Terrain | `Landscape` (`engine/Poseidon/World/Terrain/Landscape*.cpp`), submitted from `Landscape::Draw` | `World` and gameplay query `SurfaceYAboveWater`; WGPU consumes terrain batches through `EngineWgpu` and Rust renderer FFI. |
| Weather | `Landscape` weather state and wind (`GetWind`) | Gameplay/world reads use `GLandscape`; WGPU grass and water consume the derived wind as rendering input. |
| Water | Engine water submissions in `EngineWgpu`, Rust water simulation and shaders in `engine/WgpuRenderer/rust/src/water/` | Water is renderer-local in Preview 0. Gameplay water/contact reads remain in `Landscape` and simulation collision paths. |
| Grass | `Landscape::DrawGround` signals grass; `EngineWgpu::SetGrassParams` / `SubmitGrass` submit the frame | Rust grass passes own placement and draws. Live wind and vehicle/rotor interaction are read-only renderer inputs. |
| Smoke | `SmokeSource` / `Smokes` in graphics rendering effects, attached by world objects such as `Fireplace` | World objects own emission; graphics owns presentation. Smoke visual settings must not alter gameplay visibility semantics. |
| Physics and collision | Legacy simulation and collision code under `engine/Poseidon/World/Simulation/` | `Simul.cpp` owns vehicle/contact simulation; `Collisions.cpp` owns geometry queries and hit/visibility interaction. No external backend is selected. |
| Zeus | `DebugOverlay`, `CameraVehicle`, `AICenter`/`AIGroup`, and `NetworkManager` | Detailed ownership and known limits: `docs/architecture/zeus-debug-tools.md`. |

## Guardrails

- Terrain, weather, and collision remain world-owned sources of truth. Renderer
  mirrors must not become gameplay authority.
- WGPU grass, water, smoke, and post-processing are visual consumers. Their
  quality settings may not change collision, cover, hits, AI navigation, or
  water submersion.
- Zeus operates through the normal world, AI, and network ownership chain. It
  must not acquire a parallel entity or replication system.
- A future external physics decision is gated by `PHY-GATE-0`; this map records
  the current path and is not an endorsement of a replacement backend.
