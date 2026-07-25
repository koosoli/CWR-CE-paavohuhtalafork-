---
kind: dependency_management
name: Multi-Tool Dependency Management (vcpkg, Cargo, uv)
category: dependency_management
scope:
    - '**'
source_files:
    - vcpkg.json
    - Cargo.toml
    - Cargo.lock
    - apps/tools/BlenderAddon/pyproject.toml
    - apps/tools/BlenderAddon/uv.lock
    - thirdparty/README.md
---

This repository manages dependencies through three parallel toolchains, each scoped to its language ecosystem:

1. **C/C++ dependencies via vcpkg**
   - Central manifest at `vcpkg.json` declares all C/C++ third-party libraries (catch2, cjson, curl[ssl], glslang, cli11, stb, mimalloc, zstd, sdl3>=3.4.10#1, openal-soft, opus, libogg, libvorbis, enkits, imgui with sdl3/opengl3/freetype features, and platform-scoped spdlog).
   - A `builtin-baseline` pins the vcpkg registry commit for reproducible builds.
   - An `overrides` section forces specific versions of catch2 (3.5.2), cli11 (2.4.0), mimalloc (2.2.4), and spdlog (1.5.1) across the tree.
   - Platform-specific feature toggles are used (e.g., spdlog wchar only on Windows).
   - Custom patches are applied through a vcpkg overlay port for `openal-soft` under `cmake/vcpkg-overlay-ports/openal-soft/`, containing `portfile.cmake`, `vcpkg.json`, and diffs (`devendor-fmt.diff`, `fix-mixer-uaf-on-source-free.patch`, `pkgconfig-cxx.diff`).
   - CMake toolchain files under `cmake/toolchains/` and triplets under `cmake/vcpkg-triplets/` configure Clang-based cross-compilation targets (linux-x64, win-x64, sanitizer variants).

2. **Rust workspace via Cargo**
   - Root `Cargo.toml` defines a workspace with resolver 3 and five members: `engine/Trident`, `engine/WgpuRenderer/rust`, `mserver/Archive`, `mserver/CLI`, `mserver/Client`, `mserver/MasterService`.
   - `Cargo.lock` is committed, pinning every transitive dependency with checksums from crates.io — ensuring deterministic builds across platforms.
   - Custom profiles (`dbg`, `rwdi`, `rel`) extend dev/release with debug info or release optimizations.

3. **Python tooling via uv + pyproject.toml**
   - The Blender addon at `apps/tools/BlenderAddon/pyproject.toml` declares runtime and dev dependencies (`pytest>=8.0`, `ruff>=0.11`) with `requires-python = ">=3.11"`.
   - `uv.lock` is committed alongside it, locking exact wheel/sdist hashes from PyPI for reproducible installs.

4. **Vendored headers**
   - `thirdparty/` contains hand-vendored headers for `glad` (OpenGL 4.5 Core loader) and `renderdoc` (MIT in-application API header), documented in `thirdparty/README.md`.

Conventions observed:
- Lockfiles are committed for both Rust (`Cargo.lock`) and Python (`uv.lock`) to guarantee reproducible CI and developer environments.
- Version pinning is explicit: vcpkg uses `overrides` and `builtin-baseline`; Cargo uses checksum-pinned lockfile; uv uses hash-locked wheels.
- Platform gating is done per-dependency (vcpkg `platform` fields, Python markers like `sys_platform == 'win32'`).
- Private/custom patches are isolated in an overlay directory rather than modifying upstream sources directly.