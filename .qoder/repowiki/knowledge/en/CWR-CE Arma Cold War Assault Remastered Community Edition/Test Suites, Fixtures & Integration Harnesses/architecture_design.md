The module is organized by testing tier rather than by feature:
- `unit/` contains Catch2-based C++ tests split into `apps/` (Evaluator, Server, Studio, Tetris) and `engine/Poseidon/` (Foundation, Core, Graphics, Audio, AI, World, UI, Network, etc.), each with its own `CMakeLists.txt` and `test_main.cpp` entry point.
- `integration/` uses the Rust Trident framework: each test is an `.sqf` script paired with a `.toml` config file, grouped by subsystem (`flows/`, `ingame/`, `multiplayer/`, `rendering/`, `scripting/`, `ui/`). Shared helpers live in `helpers/`.
- `smoke/` holds PowerShell Pester scripts that validate boot logs and configuration persistence.
- `stress/mp/` defines long-running multiplayer soak/fault-injection scenarios via `.stress` directories with `stress.toml` manifests.
- `e2e/` contains end-to-end server/browser visibility tests.
- `fixtures/` is a flat data store of game assets (P3D, PAA, PBO, RTM, audio, configs, stringtables, missions, mods) consumed by both unit and integration tests.
Dependency direction is one-way: tests depend on fixtures and the built binaries; there are no cross-dependencies between test tiers.