# Zeus debug-tools ownership map

This document records the production ownership path for the built-in Game
Master/Zeus tools. Zeus remains a developer and server-administration tool;
it is not a mission-script API or a replacement gameplay authority system.

## Entry points and ownership

| Concern | Production owner | Notes |
| --- | --- | --- |
| Dev-panel UI and input | `engine/Poseidon/Dev/Debug/DebugOverlay.cpp` (`DrawZeusTab`, `ProcessEvent`) | Owns UI state, lasso/select/edit interaction, and deferred UI actions. |
| Free-fly camera | `CameraVehicle` in `engine/Poseidon/World/Scene/Camera/CameraHold.cpp` | Native manual camera. Zeus opts into altitude speed scaling and map-bound protection. |
| World membership | `World::AddVehicle` / `AddAnimal` / `AddBuilding` | Spawns enter the normal World-owned collections; no Zeus-only scene container exists. |
| Unit AI | `AICenter`, `AIGroup`, `AIUnit` | `SpawnZeusUnit` creates the selected side's centre/group and preserves target acquisition and auto-targeting. |
| Network creation | `NetworkManager::CreateObject` | In `GModeNetware`, Zeus registers the group, newly created subgroup, and AI unit after their real ownership objects exist. |
| Rendering and selection | `Scene`, `Camera`, and `DebugOverlay` | Selection projects only tracked Zeus-spawned objects through the active scene camera; it does not modify renderer ownership. |

## Current interaction contract

- Click placement is explicit. When it is off, clicking a spawned object selects
  it; dragging from empty space makes a lasso without a modifier key.
- Shift-drag rotates the current selection. Dragging an existing selection moves
  it on mouse release, avoiding unsafe per-event infantry teleports.
- Spawn, rotate, copy, paste, move, and delete use the tracked Zeus records.
  Deletion destroys an infantry unit's AI owner before deleting the entity.
- Spawned opposing sides keep `DATarget` and `DAAutoTarget` enabled, so combat
  perception is owned by normal AI rather than special Zeus logic.

## Known limitations and next verification

- `DAMove` remains disabled on Zeus-edited infantry to avoid stale legacy move
  queues after direct placement. They can acquire and engage targets, but are
  intentionally stationary after placement.
- The network registration path is implemented but still needs the roadmap's
  dedicated two-client validation before it can be marked multiplayer-validated.
- Stress coverage, regression captures, command journalling, and replay support
  belong to `TEST-ZEU-001`, `NET-001`, `NET-003`, and later roadmap work.
