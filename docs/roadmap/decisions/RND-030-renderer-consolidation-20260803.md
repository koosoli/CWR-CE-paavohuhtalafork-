# RND-030 — reuse and consolidation across the shipped renderer systems

**Date:** 2026-08-03
**Input:** [`renderer-systems-ledger.yaml`](../renderer-systems-ledger.yaml), all twelve entries
audited against `new-renderer-infrastructure` on this date.
**Box closed:** *"Reuse strong existing work; do not implement a second system merely because
the master roadmap uses different terminology."*

> **This record was rewritten the same day it was written.** Its first version claimed that one
> missing piece — the C++ retained-scene feed — gated three completed systems, and recommended
> building it before anything else. That was wrong. The feed exists and is live. The error and
> how it happened are kept in §4, because the mechanism is the most useful thing here.

## 1. Headline

**There is no consolidation win, because there is nothing to consolidate.** No system is
implemented twice, and only one built system is sitting unused — deliberately, and for a
documented reason.

The branch is in materially better shape than its own plan documents say. The systematic error
runs *pessimistic*: plan status blocks under-report what shipped. Anyone planning work from the
plans alone will re-implement things that already run.

## 2. What is actually live

The thing that makes this hard to read off the code is that **"on by default" is decided in
three separate layers**, and you have to consult all three:

1. the **Rust** default in `lib.rs` (`std::env::var(...).unwrap_or(...)`),
2. the **C++** default in `EngineWgpu.cpp` (a member initialiser, only overridden when the
   variable is set),
3. the **application** default — `ConfigureWgpuUltraEnvironment()` in
   `apps/cwr/Game/GameApplication.cpp`, which *sets the environment variables themselves*
   before the engine is created.

Layer 3 is the one that catches people. It runs before `InitializeGraphicsEngine()`, and sets
`WGR_HDR`, `WGR_MSAA`, `WGR_PREPASS`, `WGR_INDIRECT`, `WGR_GPU_DRIVEN`, `WGR_GPU_WATER`,
`WGR_WATER_FFT` and `WGR_SHADOW_MAPS` to `1`. It sets them as *defaults* — an explicit
environment override still wins — so it is not the clobbering mechanism it has been described
as elsewhere. `GameDemoApplication` inherits `RunAfterArgumentParsing`, so both clients agree.

Consequently, in a shipped client:

- **GPU-driven rendering is LIVE**, not inert. `WGR_GPU_DRIVEN=1` reaches both halves; the C++
  retained-scene feed exists (`wgr_instance_add` from a `_gpuDriven`-gated hook), and the
  count-trim is implemented (`multi_draw_count_enabled`). The GPU-culling plan's status block
  still says "⛔ C++ feed + count-trim remain". Both have landed.
- **Foliage canopy normals are LIVE**, because they live in `vs_gpu` and the GPU-driven path
  draws.
- **The depth prepass is LIVE** and its Stage 2 is complete, which its own plan denies.

## 3. The one genuine built-but-unused system

`RSYS-SKIN-BAKE`. `WGR_SKIN_BAKE` is deliberately **not** in the ultra list, so it stays off.

That is correct as it stands, and should not be "fixed" by enabling it. Standalone the bake is
pure overhead — vertex-shader skinning is near-free for OFP's low-poly characters, and shadow
and prepass times were measured identical with it on and off. Its value is that baked rigid
geometry lets skinned soldiers be culled and indirect-drawn like static props, collapsing the
per-soldier count-1 draws across cascades, prepass and forward. That arrives with GPU-culling
**Stage 6** (skinned + transparent integration), which is not started.

So the reuse recommendation is narrow: **when Stage 6 is picked up, re-enable the bake as part
of it rather than building anything new.** The skin-bake plan already refuses to build a
standalone draw coalescer on the grounds that the GPU-culling indirect path is one — that
refusal is the pattern worth copying, and it is the clearest example of the box's intent being
honoured before the box existed.

## 4. How the first version of this record got it wrong

Worth keeping, because it is the same failure the audit was created to find.

The GPU-culling plan's status block says Stage 3 is "Rust DONE (`WGR_GPU_DRIVEN`, inert); ⛔ C++
feed + count-trim remain". I verified the *Rust* side against the branch, found
`gpu_driven_enabled` defaulting true, read the log string "inert until a scene registers" — and
then took the plan's word for the C++ half instead of checking it. One `grep` for
`wgr_instance_add` would have settled it.

Two lessons, both already in the roadmap in other words:

- Verifying one side of a boundary and trusting a document for the other is not verification.
- A default is not a behaviour. `WGR_GPU_DRIVEN` has three defaults in three layers, and the
  one that decides the shipped game is the one furthest from the renderer.

## 5. Where consolidation does not apply

No duplicated systems were found, which was the specific risk this box guarded against. The
plans overlap in ambition but not in code.

The four candidates verified as genuinely unbuilt — `forward-plus`, `screen-space-ao`,
`gpu-terrain-water-cull`, `terrain-fractal-detail` — remain the correct starting points for new
renderer work. With the prepass' Stage 2 confirmed complete, `screen-space-ao` starts from a
populated and exposed depth+normal G-buffer, which is a better position than its plan assumes.
