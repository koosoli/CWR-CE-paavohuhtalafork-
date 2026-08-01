# WGPU renderer FFI contract

The public boundary is [`../include/wgpu_renderer.hpp`](../include/wgpu_renderer.hpp).
It uses C linkage for every exported symbol; C++ conveniences in the header do not
cross the boundary.

- `WGR_ABI_VERSION` is the compatibility gate. Any incompatible ABI change must
  increment it in the header and in Rust's `wgr_abi_version()`. Before
  `wgr_create`, `EngineWgpu` passes `WgrAbiCheck`; Rust verifies its own
  structure size plus `WgrSurfaceDesc`, `WgrLogCallbacks`, and `WgrFrame` sizes
  and refuses a stale pairing. `required_features` is a bit-set negotiation:
  an engine requiring an unsupported capability is refused before renderer
  creation. `wgr_build_id()` identifies the Rust artifact by source revision
  and build profile in the startup log.
- C++ owns every input structure and slice passed to a call.  Rust borrows those
  inputs only for that call and never retains their pointer.
- Rust owns the opaque `WgrRenderer` returned by `wgr_create`; C++ releases it
  exactly once with `wgr_destroy`.  A null renderer is always accepted by the
  destruction and setter APIs where documented.
- Rust catches panics at exported entry points.  C++ does not let exceptions
  cross into Rust; its callbacks only log engine-owned text synchronously.
- Both sides keep compile-time size/alignment assertions for every shared data
  block.  Add or update assertions on both sides together with an ABI-version
  change.
- The executable build fingerprint and the staged DLL's SHA-256 are written by
  `scripts/write_preview0_manifest.py`.  That manifest is the build identity
  used when comparing a runtime log with the exact artifacts that produced it.

This contract deliberately does not provide transparent device recreation.
Preview-0 records device/surface failures and lets the engine use its normal
restart or backend-selection policy.
