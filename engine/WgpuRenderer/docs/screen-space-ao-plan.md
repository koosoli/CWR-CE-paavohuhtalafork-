# Screen-space ambient occlusion (GTAO) plan — MSAA-compatible, no TAA

**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust). **Status:** PLAN (2026-07-12).

## 0. The governing constraint: MSAA, not TAA

This engine anti-aliases with **MSAA** (`WGR_MSAA`, default 4×, HDR-gated) and has **no temporal
accumulation**. That single fact dictates the whole AO design, because the modern GTAO recipe is built
on TAA:

- Canonical GTAO spends a *tiny* per-frame budget (often **one** azimuthal slice) and relies on TAA to
  accumulate slices across frames + denoise the per-pixel dither. **We cannot do that.** Every frame
  must stand on its own.
- Therefore: **enough samples per frame to be stable**, + a **spatial (bilateral) denoise**, + a
  purely **spatial** dither chosen so the blur removes it. No temporal reprojection, no history buffer,
  no "rotate the slice by frame index." A ghost-free still *and* a ghost-free pan both fall out for
  free because nothing is temporal.
- This is the pre-TAA HBAO+blur playbook, updated to GTAO's cosine-correct integral + bent normal. At
  OFP's scene complexity the extra per-frame cost is affordable (see §9).

Everything below is written to that constraint. Where a choice exists, the temporally-stable option
wins even at some GPU cost.

## 1. Why add it (and how it splits with sky-visibility)

The baked **terrain sky-visibility** ([sky-visibility-ambient-plan.md](sky-visibility-ambient-plan.md))
is a *large-scale, positional, concave-only* AO: it darkens gorges/cove-beds/cliff-bases and nothing
else, because rolling terrain has a sky-view factor ≈ 1 and it can never brighten a convex ridge or
resolve a local fold. Screen-space AO covers the **complementary band**:

| | terrain sky-vis (baked) | GTAO (screen-space) |
|---|---|---|
| Range | far / km-scale, incl. **under-terrain** | short–medium (world-space radius) |
| Sees | the heightfield only | **everything in the depth buffer**: object↔terrain contact, rocks, foliage grounding, local folds |
| Occluders off-screen | yes | no (screen-space blind spot) |
| Bent normal | no (Stage-2 refinement) | **yes** → directional ambient |

They are disjoint, so they **compose** (multiply visibilities / min), not compete. GTAO supplies the
"shaded terrain has form" and "objects sit in the world" that sky-vis structurally cannot. Its bent
normal redirects the existing SH sky-irradiance ambient (`frame::sky_irradiance`), which is the direct
answer to "shaded slopes look flat."

## 2. Inputs — already produced by the prepass

The unconditional depth+normal prepass ([depth-prepass-plan.md](depth-prepass-plan.md)) was built as
the "SSAO normal G-buffer"; its outputs are exactly GTAO's inputs. Availability differs by MSAA state:

- **Depth (view/linearizable).** Single-sample **nearest-resolved** depth already exists:
  `depth_resolve` → `depth_sample_view` (`Depth32Float`), created for Hi-Z (`gfx3d/mod.rs` DepthResolve,
  `reduce_far:false` = nearest = the closest/front surface, which is what AO wants). On the 1× path the
  prepass depth is already single-sample. **Reuse this; do not add a depth resolve.**
- **View-space normal.** `NORMAL_FORMAT = Rg16Float`, oct-encoded (`gbuffer::oct_encode`), written by
  `fs_terrain_prepass` + the object prepass. **But the normal target is MSAA (`count: sample_count`)
  and is *not* resolved today.** GTAO runs per-pixel, so it needs a single-sample normal:
  - 1× path: the prepass normal is already single-sample — use directly.
  - MSAA path: **add a one-off normal resolve** (a tiny fullscreen/compute pass mirroring `DepthResolve`)
    writing a single-sample `Rg16Float`. Resolving oct-encoded normals: decode → average sample dirs →
    renormalize → re-encode (a raw texel average of oct codes is wrong across the wrap). Cheapest
    correct-enough variant: just take **sample 0** (per-pixel AO tolerates it; the bilateral blur hides
    the rest). Start with sample-0; upgrade to a proper average only if edges shimmer.

No new geometry passes — only (MSAA-only) a normal resolve.

## 3. The GTAO pass

A compute pass, dispatched after the prepass + its resolves, before the forward colour pass. Output:
a single-sample AO texture (Stage 1) and later a bent-normal texture (Stage 2), at render resolution
(or half, §4).

Per pixel, in view space (reconstruct position from depth + `inv_view_proj`/the projection params):

1. **Slices.** Pick `S` azimuthal directions in screen space (start `S=2–3`; no temporal rotation, so
   this is the per-frame azimuthal resolution). Rotate the whole slice set by a per-pixel **interleaved
   gradient noise** angle (spatial dither the blur removes) + offset the step start (spatial), no frame
   term.
2. **Horizon search.** For each slice, march `N` steps (start `N=8–12`) left and right within a
   world-space `radius` projected to screen; at each tap read depth, form the view-space sample, track
   the **max horizon angle** each side (cos of the angle to the horizon). A **thickness/falloff**
   heuristic rejects thin foreground occluders (the classic GTAO "sky over a thin pole" fix) and a
   distance falloff bounds the radius.
3. **Integrate.** GTAO's ground-truth per-slice integral (the arc-cosine cosine-weighted term between
   the two horizons, projected onto the pixel normal) → per-slice visibility. Average over slices.
   Optionally apply the GTAO **multi-bounce** darkening-compensation curve (a cheap per-pixel polynomial
   in albedo luminance) to fake interreflection instead of crushing to black.
4. **Bent normal (Stage 2).** Accumulate the per-slice unoccluded-direction vector; normalize → the
   average visible direction. This is the payload that makes shaded slopes directional (§6).

Radius is world-space (so AO is scale-stable), clamped to a max screen radius for cost. Reconstruct
view-space Z by linearizing the reversed-Z depth with the projection constants already in the frame
UBO.

## 4. Denoise WITHOUT TAA

The whole denoise budget is spatial. Three composable levers:

1. **Per-frame sample budget.** `S` slices × `N` steps must be *enough on their own*. `S=3, N=10` +
   IGN rotation is the pre-TAA HBAO sweet spot: noisy at the pixel level, but low-frequency once
   blurred. Tune up if banding shows; we have the headroom (§9).
2. **Bilateral blur** — the load-bearing step. A separable, **depth- and normal-aware** blur (weights
   fall off with view-Z difference and normal divergence) over ~4–8 px. Wide enough to erase the IGN
   dither, edge-aware enough not to bleed AO across silhouettes or crease normals. This replaces the
   temporal denoise entirely. Runs on the single-sample AO texture (cheap).
3. **Optional half-resolution + bilateral upsample** (perf lever, §9). Compute AO at ½×½, then a
   depth-aware joint-bilateral upsample to full res. AO is low-frequency so half-res is visually
   ~free; the upsample must be depth-aware to keep MSAA'd silhouettes crisp. Ship full-res first;
   drop to half-res only if the frame cost warrants.

Dither must be **spatial-only** (IGN over pixel xy). Explicitly *no* blue-noise-needs-temporal, no
frame-indexed rotation — those look right only under TAA and would shimmer here.

## 5. MSAA integration specifics

- **AO is computed and stored single-sample** (per *pixel*, not per *sample*). AO is low-frequency;
  per-pixel resolution is standard and correct under MSAA — MSAA still resolves geometric edges in the
  colour pass exactly as before; AO adds no geometric edges of its own.
- **The forward (multisampled) colour pass samples the single-sample AO by pixel coordinate**
  (`floor(frag_coord.xy)` / screen dims → texel). Every covered sample of a pixel reads the same AO
  value. Binding a non-multisampled texture into an MSAA render pass is legal and normal.
- **Edge behaviour.** The resolved depth is *nearest* (front surface), so an edge pixel's AO is the
  foreground's; background samples of that edge pixel read foreground AO over a 1-px band — low-freq,
  invisible in practice, and the bilateral blur's depth weighting keeps AO from bleeding across the
  gap. Acceptable; no haloing beyond ordinary SSAO.
- **Coverage.** AO is derived from the **opaque** prepass depth, so it lands on terrain + opaque
  objects + A2C foliage (which writes prepass depth). **Water is excluded** (transparent, drawn after;
  it keeps its sky-vis-scaled ambient + env reflection). 2D/UI is never touched.
- On the **1× path** (MSAA off) everything is single-sample already; the normal resolve is skipped.

## 6. Compositing — combined AO + directional ambient

Add the AO texture (Stage 2: + bent-normal texture) to the shared `group(0)` frame bindings, next to
sky-vis (`@binding(10)`), so terrain + objects sample it the same way. In the ambient term of
`terrain.wgsl` / `shading.wgsl`:

- **Combined occlusion:** `ao = gtao_ao * sky_vis_ao(xz)` (screen-space near/mid × baked far/large-
  scale). Multiply is the right model — independent occluders. Apply to the **ambient (indirect) term
  only**, never the direct sun (AO on direct light is the classic over-darkening artifact; direct
  occlusion is the shadow maps' job).
- **Directional ambient (Stage 2):** replace `sky_irradiance(n)` with `sky_irradiance(bent_normal)`,
  still × `ao`. A shaded slope near an occluder now samples sky irradiance from the *open* direction →
  it reads as shaped, not flat. This is the "definition on shaded terrain" payoff, and it reuses the
  already-landed SH ambient. The bent normal is view-space; rotate to world with `inverse(view)` (or
  carry a world-space bent normal out of the pass).

Sky-vis has no bent normal (its Stage-2 refinement), so for terrain the SH sample uses GTAO's bent
normal where GTAO has coverage and the geometric normal elsewhere — GTAO's is the finer signal.

## 7. Stages

1. **Scalar GTAO + bilateral blur, composited × sky-vis onto ambient.** MSAA normal resolve (sample-0),
   compute pass, spatial denoise, `@binding(11)` AO, multiply into the ambient of terrain + objects.
   Debug view (raw AO greyscale, like sky-vis). ImGui: enable, radius, strength, slice/step counts,
   blur width. **This is the bulk of the visible win.**
2. **Bent normal → directional SH ambient.** Add the bent-normal output + `sky_irradiance(bent_n)`
   path. Proper oct-normal resolve if sample-0 shimmers.
3. **Polish:** multi-bounce curve, half-res + bilateral upsample perf path, thickness-heuristic tuning,
   optional GTAO on the froxel/fog or objects-only fast path.

## 8. Plumbing / files

- `gfx3d/` (or a new `ao/` module): the normal-resolve pass (MSAA-only, mirror `DepthResolve`), the
  GTAO compute (`gtao.wgsl`), the bilateral blur (`gtao_blur.wgsl`), the AO texture(s) + their views.
- Inputs bound: `depth_sample_view` (nearest-resolved depth, already built) + the resolved single-
  sample normal + the frame UBO (projection constants, `inv_view_proj`, screen dims).
- `shaders/frame.wgsl`: `@group(0) @binding(11)` AO texture (+ 12 for bent normal in Stage 2) + a
  `gtao_ao(frag_coord)` / `gtao_bent_normal(frag_coord)` helper; extend the `CameraGroup` layout +
  both bind builders + `lib.rs` threading (same pattern the sky-vis `@binding(10)` used).
- Consumers: `terrain.wgsl`, `shaders/shading.wgsl` ambient — `ao *= gtao_ao(...)`, Stage 2 swaps the
  SH normal. Water untouched.
- Frame order (`lib.rs`): prepass → (MSAA) depth resolves + **normal resolve** → **GTAO compute** →
  **GTAO blur** → forward colour pass (samples AO). GTAO reads resolved single-sample targets, so it
  slots in right after the existing resolves.
- FFI/ImGui: **use a struct-based setter** (`WgrGtaoParams` + `wgr_set_gtao`, like `WgrSky`/
  `WgrTonemap`) — the sky-vis feature already ate two positional-arg ABI bugs; a struct kills that
  failure mode. New `AoSettings` in `Engine.hpp` + a DebugOverlay tab.

## 9. Performance

- Full-res, `S=3 × N=10` + a 6-px separable bilateral blur, once per frame, over an OFP-complexity
  depth buffer is cheap on a desktop GPU — this is 2010-era HBAO math at modern clocks. No temporal
  amortization is the cost of no TAA; at this scene scale it is not the bottleneck.
- Half-res compute + bilateral upsample is a 4× fill reduction held in reserve (§4.3).
- The MSAA normal resolve is one extra tiny pass, and only when `sample_count > 1`.

## 10. Risks / notes

- **No-TAA noise floor.** If `S=3,N=10`+blur still bands on grazing slopes, raise `N` before widening
  the blur (a too-wide blur washes out contact darkening — the very thing we're adding). This is the
  main tuning axis; expose slices/steps/blur in ImGui.
- **Oct-normal MSAA resolve.** Averaging oct codes across the encoding wrap is wrong; use decode→
  average→encode, or sample-0. Getting this wrong shows as sparkle on silhouettes.
- **Depth resolve is *nearest*.** Correct for AO (front surface). Do **not** reuse the *farthest*
  resolve (`depth_resolve_far`, water's) — it would place the AO receiver behind foreground edges.
- **Radius is world-space**, projected per-pixel — keeps AO scale-stable across the wildly varying view
  distances of a flight sim; a screen-space-pixel radius would balloon on distant terrain.
- **Ambient-only application.** Keep AO off the direct sun and off emissive; it modulates the SH/flat
  ambient (and, combined with sky-vis, the same term) — never the N·L or the shadow terms.
- **Compose, don't double-count with sky-vis** near the ground: both darken ambient. Multiplying two
  visibilities is correct (independent occlusion); it will *not* over-darken the way summing would, but
  watch cove-beds where both are strong — the floor (`sky_vis_floor` + a GTAO floor) keeps it off pure
  black.
```
