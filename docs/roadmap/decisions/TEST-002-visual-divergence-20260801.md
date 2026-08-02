# TEST-002 visual-difference decision record

Status: accepted Preview-0 comparison policy  
Owner: Oliver Kay  
Scope: Preview-0 original-training capture only

## Measured evidence

The reproducible original-training comparison records:

- GL33 capture: `preview0-gl33-original-training-current-capture-20260801.png`
- WGPU capture: `preview0-wgpu-mission-capture-automation-20260801-original-training-capture.png`
- Mask and metrics: `preview0-gl33-vs-wgpu-original-training-current-build-diff-20260801.{json,png}`
- Changed-pixel ratio: `0.8855425990506636`
- Mean absolute RGB delta: `44.495722268013054`

The WGPU capture is a real rendered mission frame with non-zero GPU timing and
grass-instance metrics, not a synthetic image. The difference is therefore
useful evidence of the modern renderer's lighting, HDR/tonemap, vegetation,
and water paths; it is not proof of visual parity with GL33.

## Decision

Preview 0's goal is a trustworthy WGPU build, not visual identity with GL33.
WGPU's modern lighting and terrain treatment are preferred directionally and
must not be retuned merely to reproduce GL33's tone.

GL33 comparison is a correctness oracle only. Every notable difference must be
classified as one of:

- `CORRECTNESS_BUG`: missing geometry, broken textures, incorrect colour-space
  handling, unstable output, severe lighting failure, or another defect that
  prevents the scene from rendering correctly. This blocks Preview 0.
- `EXPECTED_DIFFERENCE`: an intentional renderer difference that preserves
  scene readability and game compatibility. This does not block Preview 0.
- `DEFERRED_VISUAL_WORK`: a visible improvement worth pursuing later but not a
  correctness failure. This does not block Preview 0.

The current broad colour, HDR/tonemap, vegetation, water, and terrain-tone
differences are `EXPECTED_DIFFERENCE`. The changed-pixel ratio is retained as
evidence, not a numerical release gate. Any future comparison finding that
matches the `CORRECTNESS_BUG` definition must be recorded separately and fixed
before Preview 0 acceptance.

## Reproduction

Use the evidence paths above with `scripts/compare_preview0_captures.py` and
`docs/roadmap/visual-acceptance-profile.preview0-original-training.json`. The
profile records this classification policy rather than asserting visual parity.
`correctness-review` produces `REVIEW_REQUIRED` until an explicit reviewer
approval names the exact reference and candidate image hashes. Do not use
`--require-accepted` as a GL33 visual-identity gate. The
build/runtime fingerprint is in
`docs/roadmap/evidence/preview0-manifest.json`.
