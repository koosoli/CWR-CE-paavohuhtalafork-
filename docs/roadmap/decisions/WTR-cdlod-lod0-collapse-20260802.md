# Finding — water CDLOD selection collapses to level 0

**Date:** 2026-08-02
**Renderer:** WGPU (`engine/WgpuRenderer`)
**Observed with:** `triWaterStats`, added in the same session
**Relates to:** `WTR-030` (frequency-aware distance filtering and ocean LOD),
`engine/WgpuRenderer/docs/water-cdlod-geometry-plan.md`

## Observation

Measured from a running mission on Malden, sampled after 90 simulated frames:

```
frame=16069 nodes=24 tris=442368
lod0=24 lod1=0 lod2=0 lod3=0 lod4=0 lod5=0 lod6=0 lod7=0 lod8=0 lod9=0
```

**Every selected node is at LOD 0. No node is ever selected at any coarser
level.** The water surface is meshed at uniform maximum density — 442,368
triangles — regardless of distance.

## What this means

The CDLOD machinery is present and running: nodes are selected, the histogram
is computed, the ranges exist. But the level hierarchy never engages, so the
system pays for a multi-level structure while delivering single-level output.

The `water-cdlod-geometry-plan.md` already anticipated the mechanism, in the
diagnostic comment beside the histogram in `WaterWgpu.cpp`:

> With `_baseMult = 8` and a 32-texel leaf, `ranges[0]` may already exceed the
> draw distance, in which case every visible node is level 0 and coarse index
> buffers would buy nothing.

This measurement confirms that predicted case is the one actually occurring.

## Why this is recorded rather than fixed

Two reasons, and neither is "it looks fine".

1. **The fix is not "make the LODs engage".** If `ranges[0]` exceeds the draw
   distance, then every visible node genuinely *is* within level-0 range, and
   forcing coarser levels would reduce quality without a measured reason. The
   prior conclusion on this — that per-LOD mesh striding buys nothing here, and
   the useful lever is halving `GRID_N` instead — points at vertex density, not
   at the level selection.
2. **It is a performance change, and there is no water performance baseline.**
   Changing tessellation density without a measured before/after would be
   exactly the speculative optimisation the roadmap forbids. `TEST-WTR-001`
   exists to provide that baseline and does not exist yet.

## Guard put in place

`tests/integration/water/water_alive.test.sqf` now bounds the node count
(`> 0`, `< 4096`). That is deliberately a wide guard against the two silent
failure modes — a selection collapsing to nothing, or running away into a
frame-time cliff — not a performance assertion.

The LOD *distribution* is intentionally **not** asserted. Pinning `lod0 == 24`
would lock in the current collapse and fail the moment someone legitimately
improves the selection, which is the opposite of useful.

## Recommendation

Treat this as the concrete, measured entry point for `WTR-030`. Before changing
anything:

1. Land enough of `TEST-WTR-001` to give water a timing baseline.
2. Measure whether uniform level-0 density actually costs anything at Tier 1 —
   442k triangles may be irrelevant on the reference GPU.
3. Only then decide between reducing `GRID_N` and re-tuning `ranges[]`.

An unmeasured change here would be indistinguishable from a regression.
