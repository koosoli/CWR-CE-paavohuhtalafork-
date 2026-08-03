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

**Hypothesis rejected (2026-08-03).** The first guess was that the menu ran
*ungrabbed*, letting the OS cursor pin against a screen edge so motion in that
direction stopped producing deltas. That is wrong: `_mouseGrab` defaults to `true`
(`SDLEventWindow.hpp`), so relative mouse mode is active and the OS cursor position
is irrelevant. Recorded because it is a plausible-sounding theory that a future
reader would otherwise re-derive.

**What was found and fixed instead.** The engine has a focus-suppression design that
was never connected at either end:

- `GInput.gameFocusLost` is read in eight places — `MouseState::Update`, the six
  `InputSubsystem` `QueryKey`/`QueryAxis` guards, and the gamepad look path — and was
  **written by nothing**. It sat at 0 for the whole session, so every focus guard was
  inert.
- `SetSkipKeys(true)` was called on focus gained *and* lost, and the `SkipKeys` flag
  it wrote was **read by nothing**. Dead code that read exactly like the mechanism.

`gameFocusLost` is now armed on both focus transitions and decays over
`kFocusSettleFrames` in `ProcessMouse_SDL`; the dead `SkipKeys` path is removed. Note
what that flag gates: **aim deltas only**. Menu cursor movement and clicks still pass,
which is why arming it on the way back in is safe.

This restores intended behaviour and removes the red herring, but it is **not proven
to be the reported bug** — the symptom is about clicks not registering, and this path
never blocked clicks. Still needs a smoke test.
