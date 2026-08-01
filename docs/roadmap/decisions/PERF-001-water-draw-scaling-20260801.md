# PERF-001 — Water draw scaling observation (2026-08-01)

## Decision

Keep the full-quality WGPU water path unchanged for Preview 0.  Do not enable
the low-quality path or reduce reflection, refraction, FFT, foam, or geometry
quality in response to a single high `Water draw` sample.

## Evidence

| Capture | Render size | Camera / visible water | Water draw |
| --- | ---: | --- | ---: |
| Historical water trace (`cwr-water-1024.log`) | 800 x 600 | non-equivalent historical view | 0.792 ms in its settled sample |
| `preview0-wgpu-water-scaling-baseline-20260801` | 3441 x 1440 | original-training land-facing camera | 0.855 ms |
| Oliver Kay's Water-tab capture | native interactive run | water-dominant, nearby-water view | 15.516 ms |

The two automated captures establish that the current renderer does not carry a
constant 15 ms water cost.  The water fragment shader executes full-resolution
refraction and, for nearby surface pixels, a 20-step screen-space reflection
(SSR) depth march.  Its cost therefore scales strongly with visible water
coverage and camera distance, not simply with the simulation/update timings.

The current shader history after the earlier water performance work contains
only an inactive-cascade early-out; it did not add a new expensive fragment
path.  The later rotor-wash feature changes the interaction input, not the
water shading loop.

## Preview-0 classification

`EXPECTED_DIFFERENCE` — camera-dependent performance characteristic, not a
demonstrated visual or rendering correctness regression.

## Follow-up

If performance work is resumed, first add a fixed water-facing camera path and
compare identical captures.  Any optimization must retain the current
full-quality image; low-quality/reflection-disabling switches are not an
acceptable Preview-0 remedy.  A materially different reflection algorithm is
deferred visual/performance work unless an image comparison validates it.
