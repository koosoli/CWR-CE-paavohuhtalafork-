# WGSL Coding Rules — CWR-CE Water Renderer

These rules are derived from real bugs that have caused the game to silently exit at startup
(no window, no error printed to the console, exit code 1) because the WGPU shader validator
rejected the shader before a single frame was drawn.

**Any agent writing or editing `.wgsl` files MUST read this file first.**

---

## How WGSL Errors Kill the Game

The game loads and compiles all WGSL shaders during `InitializeGraphicsEngine()`. If any shader
fails WGSL validation, the renderer panics and the process exits with code 1 immediately —
before a window appears, before any log file is written, and before PowerShell prints any
error output. The symptom is:

```
.\ColdWarAssault.exe --render wgpu --window --dev
PS D:\...>
```

…the prompt returns instantly with nothing on screen.

**Always build and test after any WGSL edit.** Do not assume the shader is valid just because
it compiled in a previous session or looks superficially correct.

---

## Rule 1 — No 4-component swizzles on `vec2`

**ILLEGAL (crashes at startup):**
```wgsl
let v: vec2<f32> = some_vec2();
let bad = v.xxyy;   // vec4 from vec2 — WGSL REJECTS THIS
let bad2 = v.xyxy;  // same — WGSL REJECTS THIS
```

**CORRECT — construct explicitly:**
```wgsl
let v: vec2<f32> = some_vec2();
let good = vec4<f32>(v.x, v.x, v.y, v.y);  // OK
let good2 = vec4<f32>(v.x, v.y, v.x, v.y); // OK
```

WGSL only allows swizzles up to the component count of the source vector.
A `vec2` has components `xy` only; trying to read `.xxyy`, `.xyxy`, `.xyzw`, etc. is a
validation error, not a warning.

> **Background:** This bug was introduced in the WTR-038 bicubic B-spline normal filter
> (`texture_bicubic_dynamics` in `water.wgsl`). The expressions `dims_inv.xxyy` and
> `floor(uv_grid).xxyy` both operated on `vec2` variables. The fix was to compute each
> scalar coordinate separately as `hx0`, `hx1`, `hy0`, `hy1` and pass them as
> `vec2<f32>(hx, hy)` to `textureSampleLevel`. Fixed in commit `a1d122a`.

---

## Rule 2 — `textureSampleLevel` level-of-detail argument must be `f32`

The mip-level argument to `textureSampleLevel` is always `f32`, not `i32` or `u32`.

```wgsl
// CORRECT
textureSampleLevel(fft_dynamics, fft_samp, uv, layer, 0.0);
//                                                     ^^^  f32

// WRONG — will fail validation
textureSampleLevel(fft_dynamics, fft_samp, uv, layer, 0);
//                                                     ^  i32 literal — reject
```

---

## Rule 3 — `texture_2d_array` requires a layer argument everywhere

When the texture binding is `texture_2d_array<f32>`, every sample call must include the
array layer as a separate `i32` argument:

```wgsl
// CORRECT — texture_2d_array needs layer between uv and lod
textureSampleLevel(fft_dynamics, fft_samp, uv, layer, 0.0);

// WRONG — would work for texture_2d but fails for texture_2d_array
textureSampleLevel(fft_dynamics, fft_samp, uv, 0.0);
```

`textureDimensions(fft_dynamics)` on a `texture_2d_array` returns `vec2<u32>` (width, height),
NOT `vec3<u32>` — the layer count is not included in the dimensions return value.

---

## Rule 4 — No scalar × vec4 where the result is used as vec4

WGSL **does** support scalar × vector multiplication, but the type inference must be
explicit when mixing. Prefer writing out the type to avoid ambiguity:

```wgsl
// SAFE
let result = vec4<f32>(1.0, 2.0, 3.0, 4.0) * 2.0;

// RISKY — if the scalar resolves to i32 instead of f32, validation fails
let result = some_vec4 * 2; // use 2.0 instead
```

---

## Rule 5 — `const` declarations outside functions require literal values

WGSL `const` at module scope must be a pure constant expression (literal or `const`-folded).
Do not use `var` or `let` values from bindings in a module-level `const`.

```wgsl
// CORRECT
const FOAM_FREQ: f32 = 0.55;

// WRONG — cannot reference a uniform binding at const scope
const FOAM_FREQ: f32 = params.foam_intensity; // validation error
```

---

## Rule 6 — Always validate with a real build before committing

After editing any `.wgsl` file, the mandatory verification steps are:

```powershell
# 1. Build
cmake --build build/win-x64-clang-rwdi --target PoseidonGame

# 2. Deploy
Copy-Item build\win-x64-clang-rwdi\apps\cwr\Game\PoseidonGame.exe `
  "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\ColdWarAssault.exe" -Force
Copy-Item build\win-x64-clang-rwdi\engine\WgpuRenderer\wgpu_renderer.dll `
  "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\wgpu_renderer.dll" -Force

# 3. Test — game must open a window within 5 seconds
.\ColdWarAssault.exe --render wgpu --window --dev
```

If the PowerShell prompt returns immediately with no window, there is a shader validation error.
Check the Cargo/Rust build output for `wgpu` validation messages — they appear during the
`cargo rustc` phase, not at game runtime.

---

## Rule 7 — All variables in expressions must be explicitly declared

Unlike C/C++, WGSL has strict lexical scoping with no implicit or global fallback for identifiers. Using an un-scoped or un-declared identifier (e.g. `crest_depth`) causes a `naga` / `wgpu` shader composition error:

```
compose wgr_water_shader (water/water.wgsl): error: no definition in scope for identifier: `crest_depth`
```

When `wgr_create` fails during `InitializeGraphicsEngine()`, the engine logs `Wgpu: wgr_create failed; backend unavailable` and silently falls back to GL33 mode.

**Fix:** Ensure every variable used in WGSL expressions is explicitly defined with `let` or `const` in scope (e.g. `let crest_depth = smoothstep(0.40, 2.00, water_depth);`).

---

## Quick Checklist Before Committing WGSL Changes

- [ ] No swizzles wider than the source vector (`.xxyy` on `vec2` etc.)
- [ ] All `textureSampleLevel` mip arguments are `f32` literals (`0.0`, not `0`)
- [ ] All `texture_2d_array` sample calls include the layer `i32` argument
- [ ] All module-level `const` values are pure literal expressions
- [ ] All identifiers referenced in expressions are declared in scope
- [ ] Numeric literals match the expected type (use `0.0` for `f32`, `0u` for `u32`, `0` for `i32`)
- [ ] Build succeeded with no Cargo errors
- [ ] Game window opened successfully after deployment
