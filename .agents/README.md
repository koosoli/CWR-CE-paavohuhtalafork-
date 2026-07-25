# Mandatory Guidelines for AI Agents Working on CWR-CE

**ATTENTION ALL AI AGENTS:** Before attempting to modify code in this repository, you **MUST** read and strictly follow the files in this `.agents/` directory:

1. **[AGENT_BOOTSTRAP_AND_DIAGNOSTICS.md](AGENT_BOOTSTRAP_AND_DIAGNOSTICS.md)**:
   - **MUST READ FIRST**: Contains the strict diagnostic protocol for startup failures, null pointer prevention rules, and binary deployment instructions.
   - **CRITICAL RULE**: Whenever changing C++ headers (`wgpu_renderer.hpp`), FFI exports (`ffi.rs`), Rust `lib.rs`, or `WaterWgpu.cpp`, you **MUST** compile and copy **BOTH** `PoseidonGame.exe` AND `wgpu_renderer.dll` simultaneously into `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\`. Copying only one binary causes an instant `Entry Point Not Found` crash on game startup!

2. **[WGSL_CODING_RULES.md](WGSL_CODING_RULES.md)**:
   - **MUST READ BEFORE EDITING `.wgsl` SHADERS**: Details WGSL syntax rules (no 4-component swizzles on `vec2`, strict identifier scoping, `f32` LOD arguments) that prevent WGPU shader compilation panics.

3. **[CWR-CE Water System Master Plan.md](CWR-CE%20Water%20System%20Master%20Plan.md)**:
   - Technical roadmap and design specifications for the water rendering pipeline.

4. **[SMOKE_TEST_INSTRUCTIONS.md](SMOKE_TEST_INSTRUCTIONS.md)**:
   - Step-by-step verification commands and test scenes for in-game water feature testing.
