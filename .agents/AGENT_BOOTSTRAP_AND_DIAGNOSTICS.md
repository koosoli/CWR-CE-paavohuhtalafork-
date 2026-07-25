# Agent Bootstrap & Diagnostic Guidelines

This file contains mandatory operational procedures and lessons learned from past debugging sessions. **All AI agents working on CWR-CE must read and follow these rules.**

---

## 1. Diagnostics Protocol for Startup Failures

When the game fails to start, exits instantly, or falls back to GL33 mode when launched with:
```powershell
.\ColdWarAssault.exe --render wgpu --window --dev
```

### Diagnostic Steps (Mandatory):
1. **Never guess the cause without logs**:
   Run the game with `--log-file cwr.log` to capture un-truncated stdout/stderr and panic tracebacks:
   ```cmd
   cmd /c ".\ColdWarAssault.exe --render wgpu --window --dev --log-file cwr.log"
   ```
2. **Check for WGSL Shader Panics**:
   Look for lines like:
   ```text
   thread '<unnamed>' panicked at ...: compose wgr_water_shader (water/water.wgsl): error: ...
   [ERRR] [GRAPHICS] Wgpu: wgr_create failed; backend unavailable
   ```
   If present, the WGSL shader failed compilation, causing `wgpu` initialization to fail and forcing silent fallback to GL33.

3. **Check for Access Violations (0xC0000005)**:
   Look for exception logs:
   ```text
   UNHANDLED EXCEPTION 0xC0000005 at 0x...
   ColdWarAssault!Poseidon::...
   ```
   Inspect the stack trace line `#00` and line `#01` to locate the exact null dereference or memory error site.

---

## 2. WGSL Shader Development Rules

Refer to `WGSL_CODING_RULES.md` for full details. Key takeaways:
- **No 4-component swizzles on `vec2`**: `.xxyy` or `.xyxy` on a `vec2` fails WGSL validation and panics Rust at runtime.
- **Strict Variable Scoping**: All identifiers used in expressions must be explicitly declared with `let` or `const` in scope. Undeclared identifiers (e.g. `crest_depth`) break shader composition.
- **Level-of-Detail arguments**: Must be `f32` literals (e.g. `0.0`, not `0`).

---

## 3. C++ Rendering & Null Safety Rules

- **Always null-check optional scene/landscape objects**:
  Objects loaded from external asset files (such as `_horizontObject`, `_skyObject`, `_starsObject`) can be `nullptr` if the asset is missing or disabled.
  ```cpp
  // REQUIRED in all render functions:
  if (!_horizontObject)
  {
      return;
  }
  ```

---

## 4. Build, Deploy & Sync Workflow

Whenever modifying C++ or Rust/WGSL code:

1. **Build**:
   ```powershell
   cmake --build build/win-x64-clang-rwdi --target PoseidonGame
   ```
2. **Deploy BOTH Binaries**:
   ```powershell
   Stop-Process -Name "ColdWarAssault","PoseidonGame" -ErrorAction SilentlyContinue
   Copy-Item -Path "build\win-x64-clang-rwdi\apps\cwr\Game\PoseidonGame.exe" -Destination "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe" -Force
   Copy-Item -Path "build\win-x64-clang-rwdi\engine\WgpuRenderer\wgpu_renderer.dll" -Destination "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\wgpu_renderer.dll" -Force
   ```
3. **Verify Execution**:
   Verify that `ColdWarAssault.exe` opens its window and logs `Wgpu: creating renderer WGPU (Rust / wgpu)` cleanly without falling back to GL33.
4. **Git Sync**:
   Push updates to both `new-renderer-infrastructure` and `main` remote branches:
   ```powershell
   git push origin new-renderer-infrastructure
   git push origin new-renderer-infrastructure:main --force
   ```

---

## 5. CRITICAL: FFI Export & Binary Synchronization (IDIOT-PROOF RULE)

When modifying FFI exports (e.g. `ffi.rs`, `wgpu_renderer.hpp`, `lib.rs` or `WaterWgpu.cpp`):

> [!CAUTION]
> If you add or modify an FFI symbol (such as `wgr_water_set_cascade_config`) and build `PoseidonGame.exe` without copying **BOTH** `PoseidonGame.exe` AND `wgpu_renderer.dll` to the game folder, `ColdWarAssault.exe` will fail to load `wgpu_renderer.dll` at runtime and exit instantly on startup with Exit Code 1 (`Entry Point Not Found`).

### MANDATORY STEPS whenever changing FFI / C++ / Rust code:
1. **Never copy only `ColdWarAssault.exe` or only `wgpu_renderer.dll`**.
2. **ALWAYS copy BOTH files together**:
   ```powershell
   Stop-Process -Name "ColdWarAssault","PoseidonGame" -ErrorAction SilentlyContinue; Copy-Item -Path "build\win-x64-clang-rwdi\apps\cwr\Game\PoseidonGame.exe" -Destination "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe" -Force; Copy-Item -Path "build\win-x64-clang-rwdi\engine\WgpuRenderer\wgpu_renderer.dll" -Destination "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\wgpu_renderer.dll" -Force
   ```
3. **ALWAYS run a verification test to confirm the window opens before handing control back to the user**.

