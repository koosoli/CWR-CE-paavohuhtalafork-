# RND-030 — Renderer-plan reconciliation

**Date:** 2026-08-02
**Branch:** `new-renderer-infrastructure`
**Head at inventory:** `539d294`
**Author:** AI assistant (authoring agent, not a reviewer — see the ledger's `review_model`)

RND-030 requires that existing renderer plans be reconciled against the active
integration branch *before* overlapping renderer work is authorised. This is
that inventory. It changes no code.

## Headline finding

**The plan corpus systematically understates what is already implemented.**

Five plans still labelled `PLANNED` / `PLAN` describe systems that are live in
the branch today. Two more are labelled "IMPLEMENTED in the working tree
(uncommitted; pending user build)" — a caveat that has been obsolete since the
work was committed on 2026-07-09.

This is the precise failure mode RND-030 exists to prevent. An agent picking
work from these documents would conclude that cascaded shadow maps, water
CDLOD, render-parameter consolidation, sky-visibility AO and the HDR pipeline
are all still to be built, and would rebuild systems that already ship. The
roadmap's own rule applies: *do not implement a second system merely because
the plan uses different terminology.*

Nothing here is a code defect. Every discrepancy is a stale document.

## Reconciliation matrix

Verified by locating the implementation in `engine/WgpuRenderer/rust/src` (and
`engine/WgpuRenderer/*.cpp` where the C++ side owns the call site).

### Contradicted — documented as unbuilt, actually live

| Plan | Doc claim | Evidence in branch |
| --- | --- | --- |
| `cascaded-shadow-map-plan` | `PLANNED` (2026-07-08) | `MAX_CASCADES = 4` (`gfx3d/mod.rs:42`), `wgr_shadow_cascades` pass (`lib.rs:1874`), `shadow_depth.wgsl` + `gpu_driven_shadow.wgsl`, far-cascade caster handling in `gfx3d/cull.rs:1392` |
| `water-cdlod-geometry-plan` | `PLANNED` (2026-07-08) | `water/mod.rs` + `water/water.wgsl`; runtime node/triangle counts logged from `WaterWgpu.cpp:979` (observed live: `total=20 lod0=20 tris=368640`) |
| `render-params-consolidation-plan` | `PLANNED` (rev. 2026-07-12) | `WgrRenderParams` (`ffi.rs:589`) with a locked 368-byte ABI assert (`ffi.rs:1026`) |
| `sky-visibility-ambient-plan` | `PLAN` (rev. 2026-07-12) | `WgrSkyVisibility` (`ffi.rs:594`), `terrain_set_sky_visibility` (`ffi.rs:1899`) |
| `hdr-pipeline-plan` | `FINALIZED` — design locked, "implementation staged" | `bloom.rs`, `bloom.wgsl`, `exposure.wgsl`, HDR render targets |

### Stale caveat — committed since, wording obsolete

| Plan | Doc claim | Reality |
| --- | --- | --- |
| `bindless-textures-plan` | "IMPLEMENTED in the working tree (2026-07-08, **uncommitted**; pending user build)" | Committed. Present in `ffi.rs`, `gfx3d/cull.rs`, `gfx3d/mod.rs`, `gpu_driven*.wgsl` |
| `depth-prepass-plan` | "Stage 1 IMPLEMENTED (2026-07-08, **uncommitted**)" | Committed. Present in `gfx3d/cull.rs`, `cull.wgsl`, `hiz.rs` |

### Accurate — claim matches the branch

| Plan | Claim | Verified |
| --- | --- | --- |
| `forward-plus-plan` | `PLANNED` | No implementation. Correct. |
| `screen-space-ao-plan` | `PLAN` | No implementation. Correct. |
| `gpu-terrain-water-cull-plan` | "planning / analysis, no code yet" | No implementation. Correct. |
| `terrain-fractal-detail-plan` | `PLANNED` | Only an unrelated heightmap-subdivision mention in `terrain/mod.rs:875`; the plan's detail-normal work is absent. Correct. |
| `compute-skin-bake-plan` | Phase 1 implemented, default **OFF** via `WGR_SKIN_BAKE` | Gate present at `gfx3d/mod.rs:1250`, VS-skinning fallback at `:3610`. Correct. |
| `gpu-culling-and-depth-plan` | `IN PROGRESS`, stages 1–3 live | `cull.rs`, `cull.wgsl`, `hiz.rs`, `cull_debug.wgsl`. Correct. |
| `procedural-sky-plan` | `IN PROGRESS`, stages 0+1 landed | Broad sky implementation present. Correct. |
| `foliage-translucency-plan` | Stages 1–3 + §9 distant forest | Implementation present. Correct. |
| `water-rendering-plan` | `REVISED`, stage 1 landed | Kept current (last touched 2026-08-01). Correct. |

## Ledger mapping

RND-030 asks that plan phase names be mapped into the authoritative ledger.
**That mapping is currently empty.** `docs/roadmap/status-ledger.yaml` contains
only the six Preview-0 blockers (`PERF-001`, `CORE-005`, `CORE-NEG-001`,
`CORE-NEG-002`, `RND-005A`, `TEST-002`). None of the renderer plans above
corresponds to a ledger ticket, so none of this shipped renderer work has a
tracked owner, verification commit, or evidence.

That is a gap to close deliberately, not silently: the shipped systems listed
in the first table are, in ledger terms, invisible.

## Currency signal

Doc freshness tracks whether a system is under active work, not whether it is
implemented:

- Last touched **2026-07-09** (`8645073`, a bulk commit): 10 of the 24 docs,
  including every contradicted entry except the two revised on 2026-07-12.
- Kept current: `water-rendering-plan`, `ffi-contract`,
  `runtime-capability-matrix` (2026-08-01), `water-interaction-emitters`,
  `water-spectral-core` (2026-07-22), `procedural-sky-plan` (2026-07-15).

Water and FFI documentation is maintained. The 2026-07-09 cohort is not, and
that cohort is where every contradiction sits.

## Recommendations

1. **Correct the five contradicted status lines in place.** Per RND-030, mark
   them reconciled rather than deleting them — the design rationale in those
   documents is still worth reading even though the status line is wrong.
2. **Do not treat a `PLANNED` status line as authority** for what to build.
   Verify against the branch first; this inventory is the current answer.
3. **Create ledger tickets for the shipped renderer systems** before
   authorising overlapping work, so they acquire owners and evidence.
4. **`forward-plus`, `screen-space-ao`, `gpu-terrain-water-cull` and
   `terrain-fractal-detail` are the genuinely unbuilt candidates.** Any future
   renderer work should start from those four, not from the contradicted five.

## Scope note

This inventory verifies that a system *exists* in the branch. It does not
assess quality, measure performance, or confirm each plan's individual staged
acceptance criteria. Those belong to the per-system tickets recommended above.
