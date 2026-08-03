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
| `cascaded-shadow-map-plan` | `PLANNED` (2026-07-08) | ⚠️ **Overstated — corrected 2026-08-03, see below.** `wgr_shadow_cascades` pass (`lib.rs:1884`), `shadow_depth.wgsl` + `gpu_driven_shadow.wgsl`, far-cascade caster handling in `gfx3d/cull.rs:1392` |
| `water-cdlod-geometry-plan` | `PLANNED` (2026-07-08) | `water/mod.rs` + `water/water.wgsl`; runtime node/triangle counts logged from `WaterWgpu.cpp:979` (observed live: `total=20 lod0=20 tris=368640`) |
| `render-params-consolidation-plan` | `PLANNED` (rev. 2026-07-12) | `WgrRenderParams` (`ffi.rs:589`) with a locked 368-byte ABI assert (`ffi.rs:1026`) |
| `sky-visibility-ambient-plan` | `PLAN` (rev. 2026-07-12) | `WgrSkyVisibility` (`ffi.rs:594`), `terrain_set_sky_visibility` (`ffi.rs:1899`) |
| `hdr-pipeline-plan` | `FINALIZED` — design locked, "implementation staged" | `bloom.rs`, `bloom.wgsl`, `exposure.wgsl`, HDR render targets |

#### Correction (2026-08-03) — the shadow row was too strong

This report is kept as written, per its own recommendation 1, with the correction recorded
rather than the row rewritten.

`cascaded-shadow-map-plan` does not belong in "documented as unbuilt, actually live" without
qualification. A cascaded shadow map system is live, but most of it predates the plan, and the
plan's own **Tier 1** is half done:

| Tier 1 item | State |
| --- | --- |
| Decouple shadow distance from the 250 m clamp | Landed (`Engine.hpp:826`, 400 m default) |
| `MAX_CASCADES` 4 → 8 at 2048² | **Not landed** — still 4 (`gfx3d/mod.rs:42`) |
| Retune and expose `splitCoef` / `distanceCoef` | Landed (`DebugOverlay.cpp:2755`, `:2761`) |
| Closed-caster front-face culling | No evidence found |

The original row cited `MAX_CASCADES = 4` as evidence the plan had shipped. That is the value
Tier 1 item 2 exists to change, so it evidences the opposite. The lesson generalises: "a symbol
the plan mentions exists in the branch" is not the same claim as "the plan landed", and this
inventory should be read as the former unless a row says otherwise. Tiers 2 and 3 were not
audited by either pass.

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

## Water — a different problem

RND-030 also asks for dependencies on water to be recorded. Water turns out to
have the opposite documentation failure to the renderer plans above.

The renderer plans carry a status line that is *wrong*. **The Water System
Master Plan carries no status at all**: 113 `WTR-` phase IDs and zero
completion markers. Nothing in it distinguishes a shipped phase from an
untouched one.

Three partial sources exist, and none of them is authoritative:

| Source | Coverage |
| --- | --- |
| `WTR-` tags in `engine/` source | 28 phase IDs |
| Reports under `docs/wtr-*.md` | 13 phase IDs (all committed 2026-07-25) |
| The master plan itself | 113 phase IDs, no status |

Cross-referencing them gives three concrete facts:

1. **Five phases exist in code but not in the plan** — `WTR-036C`, `WTR-037`,
   `WTR-038`, `WTR-072`, `WTR-074`. Work was done outside the documented phase
   structure, consistent with water having been driven by visual priority
   rather than plan order.
2. **Four phases have a report but no code tag** — `WTR-013`, `WTR-050`,
   `WTR-060`, `WTR-070`.
3. **86 plan phases have neither a code tag nor a report.**

### That third number is not a work estimate

Checking fact (2) against the source shows why. `WTR-050` (optics),
`WTR-060` (solver) and `WTR-070` (wakes) all have substantial implementations —
absorption/scattering terms in `water/water.wgsl` and `water/mod.rs`, the
spectrum solver across `water/fft.rs` and `fft_spectrum*.wgsl`, wake and
interaction handling in `water/interaction.rs`, `interaction.wgsl` and
`foam.wgsl` — while carrying no `WTR-` tag at all.

**Absence of a tag is therefore not evidence that a phase is unbuilt.** Tag
coverage is partial and inconsistent. The 86 figure is an upper bound on
remaining water work, not a count of it, and it must not be read as a backlog.

### The actual finding

**There is currently no reliable way to determine water progress from this
repository.** The plan has no status, the code tags are partial, and the
reports cover a subset. Any estimate of what water work remains is guesswork
until one of those three becomes authoritative.

The cheapest fix is to give the master plan the status column it lacks, seeded
from the code tags and reports, and to keep tagging new water work with its
phase ID. That is a water-owner decision, not something to infer here — which
is why this report records the gap rather than inventing statuses.

## Dependency map

RND-030 asks for dependencies between the renderer systems to be recorded.
These are read off the code, not off the plans.

### Frame pass order

Sequential `push_debug_group` markers inside `Renderer::render_frame`
(`lib.rs`), which is the encode order:

```
wgr_shadow_cascades  ->  wgr_sky  ->  wgr_depth_prepass  ->  wgr_water
                     ->  wgr_cloud_composite  ->  wgr_overlay
```

`wgr_hdr_resolve` and `wgr_tonemap` are encoded separately in
`Renderer::run_tonemap`.

### Water is the most heavily coupled system

Its bind group (`water/water.wgsl`, group 1) consumes the output of six other
subsystems. This is the concrete dependency list RND-030 asks for, and it
explains why water work reaches so far across the renderer:

| Water binding | Depends on |
| --- | --- |
| `scene_depth` | depth prepass |
| `scene_color` | opaque scene colour (refraction) |
| `sky_env` | procedural sky |
| `planar_color` | planar reflection pass |
| `seabed_heightmap` | terrain |
| `fft_displacement` / `fft_dynamics` / `fft_auxiliary` | water FFT solver |
| `interaction_field` | water interaction emitters |
| `foam_history` | foam accumulation |

Two consequences worth carrying into any future scheduling:

1. **Water sits downstream of depth prepass, sky, terrain and planar
   reflection.** Changing any of those four can change how water looks without
   touching a line of water code — so a water visual regression is not
   automatically a water bug.
2. **The three plans with no implementation at all** (`forward-plus`,
   `screen-space-ao`, `gpu-terrain-water-cull`) all sit upstream of, or
   alongside, this chain. `gpu-terrain-water-cull` in particular names water
   directly, so it should be scheduled with awareness of the bindings above
   rather than as an isolated terrain task.

## Scope note

This inventory verifies that a system *exists* in the branch. It does not
assess quality, measure performance, or confirm each plan's individual staged
acceptance criteria. Those belong to the per-system tickets recommended above.

The renderer-plan findings rest on locating a named symbol or shader for each
claim, which is positive evidence. The water section additionally reports
*absence*, which is weaker: it is bounded by tag coverage, and is labelled
accordingly above.
