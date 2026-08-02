# Renderer entry-point and FFI ownership map

## Startup path

1. `GameApplication::CreateAndSetGraphicsEngine` calls
   `CreateEngineWithParams`.
2. `GraphicsInitBridge.cpp` resolves the requested backend through the graphics
   backend factory. Dedicated-server and unsupported paths select `EngineDummy`;
   explicit WGPU selects `EngineWgpu`.
3. `EngineWgpu::EngineWgpu` creates the SDL surface descriptor, validates the
   C++/Rust ABI, and calls `wgr_create`.
4. Rust owns the opaque `WgrRenderer` and all wgpu device, queue, surface,
   pipeline, shader, and GPU resource lifetime behind it.

## Per-frame ownership

- C++ `EngineWgpu::FinishDraw` collects engine-owned cameras, geometry, terrain,
  water, grass, lights, commands, and debug-overlay batches into `WgrFrame`.
- `wgr_render_frame` borrows those frame slices only for the call. Rust must not
  retain C++ pointers after the function returns.
- Rust renders and presents. C++ clears its per-frame submission vectors after
  the call and remains the owner of scene/world objects and legacy engine state.

## ABI and failure boundary

- Public layouts and ABI feature bits are defined in
  `engine/WgpuRenderer/include/wgpu_renderer.hpp`.
- Rust exports are implemented in `engine/WgpuRenderer/rust/src/ffi.rs`; panic
  containment prevents Rust unwinding across the C ABI.
- `EngineWgpu` compares `WGR_ABI_VERSION`, required structure sizes, and feature
  bits before `wgr_create`. A mismatch refuses WGPU startup explicitly rather
  than falling back silently to GL33.
- C++ owns the SDL window and log callback lifetime; Rust may invoke the callback
  only while the renderer exists. `EngineWgpu` destroys meshes, then calls
  `wgr_destroy`, then destroys the SDL window.

## Key production call sites

| Concern | C++ entry | Rust/FFI entry |
| --- | --- | --- |
| ABI negotiation | `EngineWgpu::EngineWgpu` | `wgr_abi_version`, `wgr_abi_validate` |
| Renderer creation | `EngineWgpu::EngineWgpu` | `wgr_create` |
| Frame submission | `EngineWgpu::FinishDraw` | `wgr_render_frame` |
| Screenshot / diagnostics | `EngineWgpu::Screenshot` and capability queries | `wgr_screenshot_take`, `wgr_get_runtime_capabilities` |
| Shutdown | `EngineWgpu::~EngineWgpu` | `wgr_destroy` |

The detailed ABI ownership contract remains
`engine/WgpuRenderer/docs/ffi-contract.md`; this map records how that contract
connects to the executable startup and render path.
