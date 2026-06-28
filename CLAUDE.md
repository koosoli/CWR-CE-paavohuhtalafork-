# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

The engine and game source for *Arma: Cold War Assault* (released 2001 as *Operation Flashpoint: Cold War Crisis*), codename **Poseidon** — the original technology lineage that later became Real Virtuality, Arma, and Enfusion. Bohemia Interactive released it as GPL-3.0-or-later (with Section 7 additional terms). The code is a ~2001 C++ engine modernized to C++20, built with CMake + Clang for Windows x64 and Linux x64.

Important separations: the **source code** here is GPL; the **name/trademarks** ("ARMA", "Operation Flashpoint") are *not* granted (a fork must be renamed); the **game data** (models, textures, sounds, missions) is *not* in this repo — it ships separately under APL-SA, and the compiled binaries need it to run (free Demo data is on Steam).

## Build

Configure + build with a CMake preset (presets live in `cmake/presets/`, defined relative to `CMakePresets.json`):

```sh
cmake --preset win-x64-clang-rwdi          # or linux-x64-clang-rwdi
cmake --build build/win-x64-clang-rwdi
```

Build directory always mirrors the preset name: `build/<preset>`. Key presets: `*-clang-dbg` (Debug), `*-clang-rwdi` (RelWithDebInfo, the default workhorse), `*-clang-rel` (Release). Sanitizer/fuzzer presets: `*-clang-san` (ASan+UBSan), `linux-x64-clang-tsan` (TSan), `*-clang-fuzz` (libFuzzer, sets `POSEIDON_BUILD_FUZZERS=ON`). There is also `linux-x64-steamrt4` for the Steam Runtime.

Requirements: a **Clang** toolchain (the toolchain files in `cmake/toolchains/` chainload it), **Ninja**, **vcpkg** (`VCPKG_ROOT` must be set; deps are in `vcpkg.json`), and **ccache** (referenced by the base preset as the compiler launcher). Distributables are staged into `dist/<arch>-<platform>-<suffix>/` on build.

## Test

Unit tests (Catch2 + ImGui Test Engine) build with the normal build and run via CTest:

```sh
ctest --test-dir build/linux-x64-clang-rwdi --output-on-failure
ctest --test-dir build/linux-x64-clang-rwdi -R "<test name>" --output-on-failure   # single test/regex
```

Test trees live under `tests/` — `unit/` (C++), `integration/` (SQF-driven, run by Trident), `smoke/` (Pester boot-log checks), `fixtures/` (binary test data: P3D/PAA/PBO/RTM). Unit suites are split by area: `PoseidonFoundationTests`, `PoseidonCoreTests`, `PoseidonTests` (engine; filter subsets with Catch2 tags like `[config]`, `[graphics]`), `PoseidonServerTests`, `PoseidonEvaluatorTests`, `PoseidonTetrisTests`.

**Integration tests** need game data and the Trident CLI (`tri`, a Rust tool in `engine/Trident/`):

```sh
cargo build --manifest-path engine/Trident/Cargo.toml
# copy .trident.env.example -> .trident.env, set OFPR_GAME_DIR (built binaries) and OFPR_DATA_DIR (Demo data)
./engine/Trident/target/debug/tri test -j6 --retries 2 tests/integration
```

The recommended local layout puts Demo data in `packages/Demo` (the whole `packages/` tree is gitignored).

## Lint / format

Formatting and static analysis are wired as CMake custom targets (build them like `cmake --build build/<preset> --target Format`):

- `Format` / `FormatFix` — clang-format check / apply (config in `.clang-format`: 4-space indent, Allman braces, 120 cols).
- `Tidy` / `TidyFix` — clang-tidy (config in `.clang-tidy`).
- `PythonLint` / `PythonLintFix` — ruff (via `uv`) for the Blender addon, only if `uv` is present.
- `Lint` / `LintFix` — combined C++ (+ Python) convenience targets.
- `FileSize` — warns at >3000 lines, errors at >5000.

The top-level `CMakeLists.txt` sets a large block of `-Wno-*` suppressions; these are deliberate for this legacy codebase (verified ~11,600 warnings without them). Don't "fix" warnings by re-enabling them globally.

## Rust components

Two independent Cargo workspaces, separate from the CMake build:

- `engine/Trident/` — `tri`, the test runner / integration tool.
- `mserver/` — master-server service and tooling crates (`Archive`, `CLI`, `Client`, `MasterService`). Standard `cargo fmt` / `cargo clippy` / `cargo test` / `cargo build`.

## Architecture

The build is a stack of static libs (`engine/`) consumed by app targets (`apps/`). Top-level `CMakeLists.txt` lists the subdirectory order; `engine/README.md` and `apps/README.md` have the authoritative target tables.

**Engine libraries:**
- **Poseidon** (`engine/Poseidon/`) — the whole engine in one lib: AI, audio, entities, world/scene/terrain, scripting, foundation/runtime, IO, graphics *interface* (not backend), UI, networking, locale. Note: `engine/Evaluator/` and `engine/Random/` source files are compiled *into* Poseidon (see `engine/Poseidon/CMakeLists.txt`), not built as separate libs.
- **PoseidonGL33** / **PoseidonOpenAL** — the concrete OpenGL 3.3 and OpenAL backends, behind the engine's graphics/audio interfaces; client apps link these, tools generally don't. Backend selection goes through `engine/Poseidon/Graphics/GraphicsEngineFactory.*`.
- **PoseidonFormats** (`engine/PoseidonFormats/`) — a C-API shared lib (DLL) exposing P3D/PAA/PBO/RTM readers; consumed by the Blender addon.

**Apps** (`apps/`): `cwr/Game` + `cwr/GameDemo` (GUI clients sharing `cwr/GameBase`), `cwr/Server` (dedicated server), and tools under `tools/` (`PoseidonTools` asset CLI, `PoseidonEvaluator` SQF CLI, `PoseidonStudio` ImGui shell, `TcPbo`→`pbo`, `TcLister`→`poseidon`). `tetris/` and `fuzzers/` are sample/harness targets.

**Foundation** (`engine/Poseidon/Foundation/`) is the base layer everything sits on — custom containers, strings (`RString`), math, memory allocators, threading, logging, module/framework glue. Most substantial targets precompile `Foundation/PoseidonPCH.hpp` as a PCH; building with `-DPOSEIDON_DISABLE_PCH=ON` is the way to audit include self-containment.

**Scripting (SQF/SQS):** the core expression evaluator is `engine/Evaluator/express.*` (the classic OFP "GameState" interpreter, with `EvalState`, `SqsRunner`, `Validate`). The engine extends it with game commands in `engine/Poseidon/Game/Commands/GameStateExt*` and `engine/Poseidon/Game/Scripting/`. `PoseidonEvaluator` is a standalone CLI around this evaluator.

**World / entities / AI:** `engine/Poseidon/World/` owns the scene, terrain, simulation, and entity model (`World`/`GameState` live here); `engine/Poseidon/AI/` holds the agent hierarchy (`AICenter` → `AIGroup` → `AISubgroup` → `AIUnit`, plus vehicle AI and pathfinding under `Path/`). Much logic is split across `*.cpp` + `*Impl*.cpp` + `*.inc` files for one class.

**Networking:** `engine/Poseidon/Network/` is large and layered — `NetTransport*` is the low-level transport/session/voice layer; `Network*`/`NetworkServer*`/`NetworkClient*` is the gameplay messaging layer; master-server browser/publisher integrate with the Rust `mserver/` service.

**IO / file formats:** `engine/Poseidon/IO/` — PBO archives (`PackFiles`), the `ParamFile` config system (`.cpp`/preprocessor in `ParamFile/`, `PreprocC/`), streams, serialization, and the threaded `FileServer`.

### Conventions worth knowing

- `__FILE__` is rewritten to repo-root-relative on GNU-driver clang (`-fmacro-prefix-map`) so log/assert lines read `engine/Poseidon/...:NN`. clang-cl is intentionally left with absolute `__FILE__` because test helpers (`RepoPath()`) walk `__FILE__`'s parents expecting an absolute path.
- A lot of code uses `memcpy`/`memset` on non-trivial types intentionally (the `ClassIsMovableZeroed` pattern) — hence `-Wno-nontrivial-memcall`. Preserve these patterns rather than rewriting to copy constructors.
- Large classes are commonly partitioned across multiple translation units (`Foo.cpp`, `FooImpl.cpp`, `FooImplHealth.cpp`) and `.inc` includes; look for sibling files before assuming a method is missing.
