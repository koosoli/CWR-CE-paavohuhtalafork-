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

## Weather and time controls (2026-08-03)

The Zeus tab drives weather and the clock through `DebugCheats`, not through
`World`/`Landscape` directly, so the dev panel, the console commands and the tri
harness share one path.

| Control | Route |
| --- | --- |
| Overcast, Fog, Transition | `Cmd_SetWeather::InvokeWeather(overcast, fog, seconds)` |
| Weather presets | same, with fixed pairs |
| Time of day, Jump-to presets | `Cmd_SetTimeOfDay::InvokeHour(hour)` |
| Time scale | `Cmd_TimeMultiplier::SetValue` / `Get` |

Three constraints worth knowing before extending this:

- **The clock only moves forward.** The engine exposes `World::SkipTime` (relative)
  and no absolute setter, so `InvokeHour` computes the shortest *forward* delta and
  wraps past midnight. Backward skips are not known to be safe for day/night and
  lighting state.
- **The time slider applies on release, not per frame.** Applying during a drag
  would fire a skip per frame and sail past the target.
- **Overcast has no working read-back.** `Landscape::GetOvercast()` forwards to
  `Weather::GetOvercast()`, which is declared in `Landscape.hpp` but defined
  nowhere — calling it is a link error. Fog does read back
  (`Weather::GetFog()` is inline). If overcast read-back is wanted, defining that
  function is the prerequisite.

Pre-existing documentation errors corrected while doing this: `Cmd_SetWeather`
claimed fog was "left at the engine's current" (it has always been forced to 0),
and claimed there was "no public getter" for fog (`Weather::GetFog()` exists;
only the `Landscape` forwarder was missing, and is now added).

## Known issue — cursor after focus loss (reported 2026-08-03, NOT diagnosed)

Alt-tabbing to another application and back leaves the cursor unable to click game
menu items. Not yet reproduced or fixed. Ruled out so far, by reading:

- `SkipKeys` — `SetSkipKeys(true)` is called on focus *gained* as well as lost and
  is never cleared, which looks like the bug but is not: the `SkipKeys` flag in
  `InputProcessingSdl.cpp` is written and **never read**. It is dead code and
  should probably be removed.
- `IGraphicsEngine::Activate()` / `Deactivate()` — empty virtuals, no backend
  overrides.
- `DebugOverlay::WantsMouse()` — swallows mouse while the panel is open, by design,
  and `s_zeusConsumeMouseEvent` is reset at the top of every `ProcessEvent`, so it
  cannot latch.

Leading hypothesis, untested: the UI cursor is integrated from *relative* motion
(`SDL_EVENT_MOUSE_MOTION` `xrel`/`yrel`) while the OS cursor is hidden. Focus loss
disables relative mouse mode unconditionally; focus gain re-enables it only when
`_mouseGrab` is set (`SDLEventWindow.hpp`). If the menu runs ungrabbed, the OS
cursor can end up pinned against a screen edge while away, after which motion in
that direction yields no deltas and the virtual cursor cannot be steered back.
Warping the OS cursor to the window centre on focus-gain would test this.
