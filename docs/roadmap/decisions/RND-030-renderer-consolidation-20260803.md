# RND-030 — reuse and consolidation across the shipped renderer systems

**Date:** 2026-08-03
**Input:** [`renderer-systems-ledger.yaml`](../renderer-systems-ledger.yaml), all twelve entries
audited item by item against `new-renderer-infrastructure` on this date.
**Box closed:** *"Reuse strong existing work; do not implement a second system merely because
the master roadmap uses different terminology."*

## Headline

**One missing piece gates three completed ones.**

The C++ retained-scene feed — Stage 3b-3 of the GPU-culling plan — is the only thing standing
between the branch and the payoff of three systems that are already built, validated and
sitting inert. Nothing else found in the audit comes close to that leverage.

## The three that are waiting on it

| Built | State | Waiting for |
| --- | --- | --- |
| GPU cull + LOD compute (`RSYS-GPU-CULL` stage 3) | Rust done, default-on, **inert** | C++ registering a retained scene |
| Compute skin bake (`RSYS-SKIN-BAKE`) | Phase 1 done + validated, **off by default** | GPU-driven stage 6, which needs stage 3 first |
| Foliage canopy normals (`RSYS-FOLIAGE-TRANS` stage 3) | Implemented in `vs_gpu`, **not active by default** | the same feed — `vs_gpu` only draws what the retained scene registers |

Each was verified working by whoever built it. None of the three is what the game shows in a
default run. That is not three separate stalls; it is one stall counted three times.

The skin bake case is the clearest. It is switched off *on purpose* — standalone it is pure
overhead, because vertex-shader skinning is near-free for OFP's low-poly characters and shadow
and prepass times were measured identical with it on and off. Its entire value is that baked
rigid geometry lets skinned soldiers be culled and indirect-drawn like static props, collapsing
the per-soldier count-1 draws across cascades, prepass and forward. That value is unreachable
until the feed lands.

The foliage case is the least visible and the most surprising: the canopy-normal work fixes the
back-facing-card problem, was user-verified for bushes, and does not run unless someone sets
`WGR_GPU_DRIVEN`.

## Recommendation

**Build the C++ retained-scene feed before starting any new renderer system.**

This is a reuse decision, not a new feature: it is the smallest change that converts three
finished, validated systems from inert to live. Starting `screen-space-ao` or `forward-plus`
first would add a fourth system to a branch that is not yet cashing the three it has.

Two supporting notes for whoever picks it up:

- The prepass is further along than its plan admits — Stage 2 is complete, so the depth and
  normal G-buffer is populated (including foliage) and exposed. Anything consuming it starts
  from a better position than the documents suggest.
- `WGR_GPU_DRIVEN`'s two halves disagreed on parsing until 2026-08-03: Rust enabled on any
  value but `"0"`, C++ required exactly `"1"`, so `=true` produced a half-enabled system that
  logged success. Fixed, but worth knowing when reading older session notes that claim the
  path was exercised.

## Where consolidation does NOT apply

The audit found no duplicated systems — nothing is implemented twice under different names,
which was the specific risk `RND-030` was guarding against. The plans overlap in *ambition*
(the skin-bake plan explicitly refuses to build a Phase 2 draw coalescer because the
GPU-culling plan's indirect path already is one) but not in code. That refusal is the pattern
to copy.

The four candidates verified as genuinely unbuilt — `forward-plus`, `screen-space-ao`,
`gpu-terrain-water-cull`, `terrain-fractal-detail` — remain correct starting points *after* the
feed, not instead of it.

## Caveat on this analysis

The leverage claim rests on the audit's reading of `vs_gpu`'s call path and the C++ `_gpuDriven`
gate, not on a measurement. Nobody has run the branch with the feed present, because it does not
exist. If the feed lands and the three systems do not deliver, this recommendation was wrong
about the payoff, not about the dependency — the dependency is structural and is visible in the
code.
