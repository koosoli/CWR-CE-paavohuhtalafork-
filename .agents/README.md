# Mandatory Guidelines for AI Agents Working on CWR-CE

> [!CAUTION]
> **ATTENTION ALL AI AGENTS:** You MUST read and strictly obey these rules before editing any code or shaders in this repository. Failure to follow these rules breaks the game installation and causes instant startup crashes for the user!

---

## 🚨 THE IDIOT-PROOF RULES (WHAT YOU MUST ALWAYS DO)

### RULE 1: Mandatory Build & Dual Binary Deployment After EVERY File Edit
Editing any source file (`.wgsl`, `.cpp`, `.hpp`, `.rs`) in the repository **DOES NOT** automatically update the game installation. The game in `D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\` will continue running stale or mismatched binaries until you manually build and deploy.

**Whenever you modify ANY file, you MUST run this EXACT 3-step command sequence:**

```powershell
# Step 1: Rebuild PoseidonGame
cmake --build build/win-x64-clang-rwdi --target PoseidonGame

# Step 2: Deploy BOTH binaries (EXE + DLL) simultaneously to the game folder
Stop-Process -Name "ColdWarAssault","PoseidonGame" -ErrorAction SilentlyContinue; Copy-Item -Path "build\win-x64-clang-rwdi\apps\cwr\Game\PoseidonGame.exe" -Destination "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe" -Force; Copy-Item -Path "build\win-x64-clang-rwdi\engine\WgpuRenderer\wgpu_renderer.dll" -Destination "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\wgpu_renderer.dll" -Force

# Step 3: Verify execution by running with log capture
cmd /c ".\ColdWarAssault.exe --render wgpu --window --dev --log-file cwr.log"
```

> **NEVER** copy only `ColdWarAssault.exe` or only `wgpu_renderer.dll`. Copying only one binary causes an instant `Entry Point Not Found` crash on startup!

---

### RULE 2: Mandatory Verification Before Ending Your Turn
- **NEVER** tell the user a feature or bug is fixed until you have executed Step 3 above and verified that the game window launches without panicking or exiting on startup.
- Verify that `cwr.log` contains:
  `[INFO] [GRAPHICS] Wgpu: creating renderer WGPU (Rust / wgpu)`

---

### RULE 3: Diagnostic Protocol for Startup Failures
If the game exits immediately or fails to launch:
1. **DO NOT GUESS** why it failed or blame PowerShell.
2. Run with `--log-file cwr.log` and read the log output immediately.
3. Check for WGSL compilation panics:
   ```text
   thread '<unnamed>' panicked at ...: compose wgr_water_shader (water/water.wgsl): error: ...
   ```
4. Check for null dereference stack traces:
   ```text
   UNHANDLED EXCEPTION 0xC0000005 at 0x...
   ```

---

### RULE 4: Strict WGSL Shader Syntax Rules
Before editing any `.wgsl` shader file, read **[WGSL_CODING_RULES.md](WGSL_CODING_RULES.md)**:
- ❌ **No 4-component swizzles on `vec2`**: Writing `vec2.xxyy` or `vec2.xyxy` fails WGSL validation and panics Rust at runtime.
- ❌ **No undeclared variables**: Every variable referenced in a WGSL expression MUST be explicitly declared in scope (`let crest_depth = ...`).
- ❌ **No integer LOD literals**: The LOD argument to `textureSampleLevel` MUST be an `f32` (use `0.0`, not `0`).

---

### RULE 5: C++ Null Safety
Always null-check optional scene/landscape render objects before dereferencing:
```cpp
if (!_horizontObject)
{
    return;
}
```

---

## 📁 Directory Document Index

1. **[AGENT_BOOTSTRAP_AND_DIAGNOSTICS.md](AGENT_BOOTSTRAP_AND_DIAGNOSTICS.md)**: Deep breakdown of diagnostic procedures, crash tracebacks, and build/deploy scripts.
2. **[WGSL_CODING_RULES.md](WGSL_CODING_RULES.md)**: Full WGSL syntax pitfall guide and solution examples.
3. **[CWR-CE Water System Master Plan.md](CWR-CE%20Water%20System%20Master%20Plan.md)**: Technical roadmap for the water rendering pipeline.
4. **[SMOKE_TEST_INSTRUCTIONS.md](SMOKE_TEST_INSTRUCTIONS.md)**: In-game verification commands and test scenes.
