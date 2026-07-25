---
kind: configuration_system
name: Poseidon Configuration System — ParamFile-based .cfg persistence with layered CLI/AppConfig
category: configuration_system
scope:
    - '**'
source_files:
    - engine/Poseidon/IO/ParamFile/ParamFile.hpp
    - engine/Poseidon/IO/ParamFile/ParamFile.cpp
    - engine/Poseidon/UI/Settings/AudioConfig.hpp
    - engine/Poseidon/UI/Settings/DisplayConfig.hpp
    - engine/Poseidon/UI/Settings/GraphicsConfig.hpp
    - engine/Poseidon/UI/Settings/ControlsConfig.hpp
    - engine/Poseidon/Foundation/Platform/AppConfig.hpp
    - engine/Poseidon/Core/Config/EngineConfig.hpp
    - engine/Poseidon/Foundation/Common/GamePaths.hpp
    - engine/Poseidon/Network/NetworkConfig.hpp
    - apps/cwr/Game/GameApplication.cpp
    - apps/tools/Studio/StudioConfig.cpp
    - engine/Poseidon/Foundation/Common/PlayerPrefs.cpp
---

## What system/approach is used

The Poseidon engine uses a **ParamFile-based configuration subsystem** for all user-facing settings, combined with a centralized **AppConfig singleton** for command-line and process-wide runtime flags. User preferences are persisted as human-readable `.cfg` files (display.cfg, graphics.cfg, audio.cfg, controls.cfg, difficulty.cfg, mouse.cfg, gamepad.cfg, studio.cfg, prefs.cfg) under a per-application user directory resolved by `GamePaths`. The same ParamFile parser/serializer is reused across the engine for mission configs, savegames, and addon metadata.

## Key files and packages

- **ParamFile core**: `engine/Poseidon/IO/ParamFile/ParamFile.hpp`, `ParamFile.cpp` — generic key/value/array/class parser and serializer supporting text and binary formats, access modes, visibility tests, and CRC verification.
- **User-setting classes** (all follow the same Defaults / Normalize(env) / Load(path) / Save(path) pattern):
  - `engine/Poseidon/UI/Settings/AudioConfig.hpp`
  - `engine/Poseidon/UI/Settings/DisplayConfig.hpp`
  - `engine/Poseidon/UI/Settings/GraphicsConfig.hpp`
  - `engine/Poseidon/UI/Settings/ControlsConfig.hpp`
- **CLI/runtime flags**: `engine/Poseidon/Foundation/Platform/AppConfig.hpp` — singletons parsed from CLI arguments, exposing getters for display mode, network ports, mod paths, language, logging, test harness, etc.
- **Engine-level config**: `engine/Poseidon/Core/Config/EngineConfig.hpp` — global `ENGINE_CONFIG` macro-backed struct consumed across the engine (memory limits, LOD, lights, audio backend, runtime flags).
- **Path resolution**: `engine/Poseidon/Foundation/Common/GamePaths.hpp` — resolves `<user-dir>/`, `<mods-dir>`, `<workshop-dir>`, `<missions-dir>` with environment overrides (`POSEIDON_USER_CONTENT_DIR`, `POSEIDON_MODS_DIR`, `POSEIDON_WORKSHOP_DIR`) and legacy `-oldpaths` mode.
- **Network config**: `engine/Poseidon/Network/NetworkConfig.hpp` — port, bind/advertise addresses, password, master server, proxy, public/private server flags.
- **Application bootstrap**: `apps/cwr/Game/GameApplication.cpp` — orchestrates loading display.cfg → graphics.cfg → audio.cfg with eager-default-write-on-missing behavior.
- **Tool-specific configs**: `apps/tools/Studio/StudioConfig.cpp` (studio.cfg), `engine/Poseidon/Foundation/Common/PlayerPrefs.cpp` (prefs.cfg).

## Architecture and conventions

1. **Uniform setting class contract** — Each `.cfg` type exposes:
   - Public fields with sensible defaults
   - `LoadDefaults()` to reset to factory values
   - `Normalize(const Environment&)` that validates against live hardware (monitor list, available audio devices, system RAM) without persisting transient changes
   - `Load(path)` returning false on missing/unparseable files (caller chains `Load → LoadDefaults → Save`)
   - `Save(path)` via ParamFile, atomic-ish write returning I/O error status

2. **Environment abstraction** — Validation is decoupled from persistence through an `Environment` interface per config type (e.g., `DisplayConfig::Environment` lists monitors/resolutions/refresh rates; `GraphicsConfig::Environment` reports system RAM; `AudioConfig::Environment` enumerates input/output devices). This lets unit tests inject mock environments.

3. **Eager default-write policy** — On first boot, if a `.cfg` file is missing or corrupt, defaults are written immediately so users always have a hand-editable file to inspect.

4. **Persistence timing differs by domain**:
   - Display settings: only saved when the user explicitly hits Apply in the UI (prevents losing settings when a monitor is temporarily disconnected during Normalize)
   - Audio/graphics/controls: saved on page Unmount or explicit UI actions
   - Engine config: constructed once at startup from `RuntimeFlags` + `EngineState`

5. **Layered precedence**: CLI flags (`AppConfig`) override file-based settings, which override built-in defaults. Network settings can be further overridden at runtime via `SetNetwork*` functions.

6. **ParamFile format**: Human-readable text with quoted strings, arrays (`key[]={val,val}`), nested classes, access control (`access=...`), and optional binary serialization (`\0raP` magic). Used uniformly for `.cfg` user settings, mission configs, savegames, and addon manifests.

7. **Per-app isolation**: Each application (CWR game, Tetris demo, Studio tool) gets its own user directory via `getUserConfigDir(appName)`, keeping `display.cfg`/`graphics.cfg`/`audio.cfg` separate per app.

## Conventions and constraints

- **Field naming**: Zero-valued sentinels mean "use system default" (resolutionWidth/Height=0, refreshRate=0, outputDevice="", inputDevice="").
- **WindowMode enum**: Borderless is the safe default because it works on every system without enumerating modes first.
- **Volume convention**: 0..100 integer percent in files and UI; internal audio systems use 0..1 floats — translation happens at the `AudioConfig` boundary.
- **Binding format**: Controls bindings use packed `INPUT_DEVICE_* | code` integers stored verbatim for backward compatibility with the original 1.99 engine.
- **Quality presets**: Graphics tiers (Off/Low/Medium/High/Ultra) map to known bundles; touching any tier drops preset display to "Custom".
- **FPS cap**: Only valid values 0/30/60/90/120/144/240 are accepted; out-of-range rounds to nearest allowed value.
- **Memory limits**: Default -1 means auto-derived from physical RAM (80% soft / 92% hard); 0 = unlimited; >0 = explicit MB ceiling.
- **Mod discovery**: Mods are identified by either a `bin/Campaigns` folder (case-insensitive) or a `mod.json` manifest; local vs downloaded mods are distinguished by location (`ModsDir` vs `WorkshopDir`).
- **Legacy path mode**: `-oldpaths` flag enables same-folder runtime mode where configs live next to the executable instead of the user directory.