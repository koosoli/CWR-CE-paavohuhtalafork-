# Preview-0 WGPU capability matrix

The renderer records the detected values in one `wgpu capabilities:` startup
line.  This is the runtime source of truth; the Profile tab additionally shows
the live renderer identity so it cannot be confused with a requested backend.

| Capability | Preview-0 behaviour when unavailable |
| --- | --- |
| `TEXTURE_BINDING_ARRAY` plus non-uniform indexing | Renderer creation is refused: terrain requires descriptor indexing. Explicit `--render=wgpu` remains explicit and does not silently select GL33. |
| `VERTEX_WRITABLE_STORAGE` | Renderer creation is refused: GPU grass writes its instance data in compute and consumes it in the vertex stage. |
| BC texture compression | Start continues, but DXT texture uploads report failure; this is recorded as a Tier-1 incompatibility rather than silently decoding a different format. |
| `PARTIALLY_BOUND_BINDING_ARRAY` | Bindless texture arrays use padded dummy slots. |
| `INDIRECT_FIRST_INSTANCE` | GPU-driven indirect submission is disabled; rendering uses the direct draw path. |
| `MULTI_DRAW_INDIRECT_COUNT` | GPU-driven rendering uses the conservative no-op-tail multi-draw path. |
| Timestamp-query features | GPU timing rows are inert and report zero; rendering continues. In-pass timestamps are independently optional. |
| Requested MSAA scene format | The renderer logs a warning and uses 1x MSAA when the requested sample count is unsupported. |

The environment variables `WGR_HDR`, `WGR_INDIRECT`, `WGR_GPU_DRIVEN`,
`WGR_PREPASS`, and `WGR_MSAA` are intentional development/A-B controls. Their
effective state is written to the startup log. They are not compatibility
fallbacks and must not be used to disguise an unsupported adapter.
