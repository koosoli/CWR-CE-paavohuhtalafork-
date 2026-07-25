---
kind: build_system
name: CMake + vcpkg Build System with Multi-Platform Toolchains and Dockerized CI
category: build_system
scope:
    - '**'
source_files:
    - CMakeLists.txt
    - CMakePresets.json
    - cmake/presets/base.json
    - cmake/presets/windows.json
    - cmake/presets/linux.json
    - cmake/toolchains/win-x64-clang.cmake
    - cmake/toolchains/linux-x64-clang.cmake
    - vcpkg.json
    - scripts/Build.ps1
    - docker/steamrt4/Dockerfile
    - docker/papa-bear-master-service/Dockerfile
    - Cargo.toml
---

## What system/approach is used
The project uses a CMake 3.25+ build system layered on top of vcpkg for dependency management, with Ninja as the default generator. It supports both Windows (MSVC/clang-cl) and Linux (Clang) via explicit toolchain files and CMake presets. A Rust workspace (Cargo) coexists alongside the C++ codebase for the master server, wgpu backend, and Trident test runner. Docker images provide reproducible build environments for SteamRT4 Linux and the papa-bear-master-service binary.

## Key files and packages
- **Root CMakeLists.txt** — global C++20/C11 settings, feature flags, PCH configuration, clang-format/tidy targets, and subdirectory assembly
- **CMakePresets.json** — includes platform-specific preset files
- **cmake/presets/base.json** — shared base preset with vcpkg toolchain, ccache, and overlay triplets/ports
- **cmake/presets/windows.json / linux.json** — per-platform configure presets (Debug/RelWithDebInfo/Release variants)
- **cmake/toolchains/*.cmake** — Clang toolchain definitions for Windows x64 and Linux x64, plus sanitizer/fuzz/static-CRT variants
- **vcpkg.json** — dependency manifest (catch2, curl, glslang, SDL3, OpenAL, imgui, spdlog, etc.) with overrides and baseline
- **scripts/Build.ps1** — PowerShell wrapper that sets VCPKG_ROOT, LLVM tools path, then runs `cmake --preset` and `cmake --build`
- **docker/steamrt4/Dockerfile** — SteamRT4 SDK image pre-seeded with vcpkg and build tools
- **docker/papa-bear-master-service/Dockerfile** — multi-stage Rust build producing a distroless container
- **Cargo.toml** — Rust workspace defining members: engine/Trident, engine/WgpuRenderer/rust, mserver/*

## Architecture and conventions
- **Preset-driven configuration**: All builds go through named CMake presets (`win-x64-clang-dbg`, `linux-x64-clang-rwdi`, `steamrt4-*`) which pin generator, toolchain file, vcpkg triplet, and binary directory layout under `build/<preset>`.
- **Toolchain separation**: Platform-specific compiler/linker flags live in `cmake/toolchains/`. Sanitizer and fuzzing toolchains are provided separately (`*-san.cmake`, `*-fuzz.cmake`).
- **vcpkg integration**: The base preset chains to `vcpkg.cmake` and enables overlay triplets and ports so the project can patch dependencies (e.g., openal-soft patches).
- **Multi-language workspace**: C++ targets are built via CMake; Rust crates are managed independently via Cargo workspace. The CMake root optionally detects `cargo` and conditionally includes the WGPU renderer.
- **Dockerized reproducibility**: SteamRT4 image provides a deterministic Linux build environment; the master service ships as a stripped binary in a distroless container.
- **Dev tooling baked into CMake**: Targets `Format`, `FormatFix`, `Tidy`, `TidyFix`, `PythonLint`, `FileSize` are auto-generated from collected source lists, enabling one-command lint/format across all languages.

## Conventions and constraints
- **Compiler standard**: C++20 required (`CMAKE_CXX_STANDARD 20`), C11 for C sources. Extensions disabled.
- **Per-target PCH**: Large targets opt into a shared Poseidon precompiled header (`PoseidonPCH.hpp`) to speed up rebuilds; small single-TU targets are excluded.
- **Dead-stripping enabled**: `/Gy /Gw` on MSVC and `-ffunction-sections -fdata-sections` on GCC/Clang so linkers can remove unused symbols.
- **Version embedding**: `BUILD_VERSION_TAG` cache variable feeds into generated `BuildConfig.h`; release metadata is injected at build time rather than checked in.
- **Optional features guarded by options**: `POSEIDON_ENABLE_WGPU` (auto-disables if cargo missing), `POSEIDON_BUILD_FUZZERS`, `CWR_HAS_OPENGL`, `CWR_HAS_OPENAL`, `POSEIDON_DISABLE_PCH`.
- **Dist output layout**: Binary artifacts land in `dist/<preset>/` with platform/arch suffixes derived from the preset name (e.g., `x64-windows-clang`, `x64-linux-steamrt4`).
- **CI/build scripts**: `scripts/Build.ps1` enforces that `VCPKG_ROOT` points to a valid vcpkg checkout and that LLVM tools are on PATH before invoking CMake presets.