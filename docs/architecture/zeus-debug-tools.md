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

## Resolved — "cursor after focus loss" was Zeus free-fly, not a bug (2026-08-03)

Reported as: alt-tab away and back, and the in-mission menu (Abort / Exit) stops
taking clicks while everything else works.

**Not a focus bug.** Zeus free-fly was still active, so the Zeus tab was consuming
the left click as a lasso-select instead of letting it reach the menu — see
`DebugOverlay::ProcessEvent`, which sets `s_zeusConsumeMouseEvent` on
`SDL_EVENT_MOUSE_BUTTON_DOWN` whenever `!s_visible && s_zeusCamera`. Leaving free-fly
restores menu clicks. The alt-tab was incidental: it was simply when the user next
tried to use the menu.

Worth keeping as a design note rather than deleting: Zeus deliberately claims clicks
so lasso and placement work with the dev panel closed, and it has no notion of "a UI
display is open, so let this click through". If that becomes annoying, the fix is to
skip the consume when a modal display owns input — not to change the focus handling.

Two real defects were found while chasing this, and both were worth fixing on their
own merits:

- `GInput.gameFocusLost` was read in eight places across the input system and written
  by nothing, so every focus guard was inert. Now armed on both focus transitions and
  decayed in `ProcessMouse_SDL`. It gates **aim deltas only**, so it stops the view
  whipping when you alt-tab back and never blocks cursor movement.
- `SetSkipKeys()` was called on every focus change and the flag it wrote was read by
  nothing. Removed — it read exactly like the mechanism while doing nothing, which is
  what made this expensive to investigate.

The focus-state diagnostic added during the investigation is kept: every transition
logs `appActive`, `appPaused`, `appIconic`, `mouseGrab`, SDL's actual relative-mouse
state and `keepFocus`. Cheap, and the next report of this shape starts with data.

