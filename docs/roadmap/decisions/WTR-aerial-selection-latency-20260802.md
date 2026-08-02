# Finding — water disappears for hundreds of frames after a large camera move

**Date:** 2026-08-02
**Renderer:** WGPU (`engine/WgpuRenderer`)
**Observed with:** `triWaterNodeCount` / `triWaterStats`
**Relates to:** `WTR-001` (aerial repetition, lifecycle reset), `WTR-030`
(frequency-aware distance filtering and ocean LOD),
[`WTR-cdlod-lod0-collapse-20260802.md`](WTR-cdlod-lod0-collapse-20260802.md)

## Observation

On Malden, moving the player (and so the camera) from ground level to 800 m and
sampling the water CDLOD selection:

| Sample point | Selected nodes |
| --- | --- |
| Ground, settled | **28** |
| +60 frames at 800 m | **0** |
| +180 frames at 800 m | **0** |
| +420 frames at 800 m | **5** |
| Back at ground, +180 frames | **24** |

**For roughly 400 frames after the move there is no water meshed at all**, and
when it returns it returns sparsely. The recovery does happen — this is latency,
not a permanent failure — and returning to ground restores a normal count.

## Why it matters

A teleport is an extreme input, but it is the same code path a fast aircraft or
a camera cut exercises more gently. The visible symptom would be water absent or
popping in during rapid altitude changes, which is a look problem rather than a
crash, and therefore easy to miss without instrumentation.

It also compounds the LOD-0 collapse recorded separately: the selection already
never uses coarse levels, so at altitude it has to populate a large area at the
finest level or nothing at all.

## What is not yet known

- Whether the latency is the CDLOD tree updating incrementally per frame, the
  terrain/bathymetry streaming the selection depends on, or a range calculation
  that only converges as the camera settles.
- Whether it scales with distance moved or is a fixed per-frame budget.
- Whether the same latency appears on a gradual climb, which is the case players
  actually see. **This has not been measured, and the teleport result should not
  be assumed to transfer.**

## What was done

`tests/integration/water/water_aerial_lod.test.sqf` guards the two hard
failures — water never returning, and an unbounded node count — with a frame
budget generous enough to cover the measured latency.

It deliberately does **not** assert a tight recovery time. That would fail today
and bake the defect in as expected behaviour. The budget is the thing to tighten
once the selection improves.

A first draft of that fixture asserted immediately after the move and passed,
because the harness retries assertions for two minutes and water recovered
inside that window. It hid the very behaviour it was written to observe; the
committed version samples on an explicit frame budget instead.

## Recommendation

Treat this as a measured entry point for `WTR-030`, alongside the LOD-0
collapse. Before changing anything, measure the gradual-climb case — if water
keeps up with a climbing aircraft, this is a teleport-only artifact and much
lower priority than the node counts suggest.
