# Interior sky visibility (LIT-020) — plan

**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust).
**Status: IMPLEMENTED, then PARKED default-OFF (2026-08-05) — tester unsatisfied.** Foliage
improved; interiors still show shadow patches with hard, geometry-unrelated edges after two rounds
of fixes. Parked on the owner's instruction. The durable fix is the per-model bake (§3c / §3d), not
another tuning pass — see the roadmap's LIT-020 entry for why the residual artifact is structural
to sampling a camera-space depth grid.
**Roadmap:** `LIT-020 — Geometry-aware interior sky visibility — REQUIRED outcome`.
**Revised 2026-08-05:** hardware ray tracing ruled out (§3F). Shipped as a **five-direction**
map — zenith plus four tilted (§3b), after §3c's resolution objection turned out to be
extent-dependent and wrong at the box size actually used (§3c-bis). The per-model bake (§3c)
remains the Stage 2 target for full quality.

## 1. The problem, stated precisely

The renderer has no idea it is indoors. Every opaque surface gets the full outdoor SH sky
irradiance, so a windowless room is lit as brightly as the field outside it — which is why
interiors read flat and why turning GTAO on barely changed them.

The two AO systems already shipped cannot fix this, and it is worth being exact about why:

| | reach | sees | why it cannot do interiors |
|---|---|---|---|
| terrain sky-vis (baked) | km-scale | the heightfield only | a building is not in the heightmap |
| GTAO (screen-space) | ~2 m | the depth buffer | a room is bigger than the radius, and the roof is often off-screen |

Both answer "how much is this point locally occluded". Neither answers "can sky light reach this
point at all", which is a *global visibility* question about geometry the camera may not be
looking at.

## 2. Acceptance (from the roadmap)

- [ ] Porch partly dark.
- [ ] Window-adjacent room receives light.
- [ ] Deep room is dark.
- [ ] Sealed bunker has no unexplained ambient skylight.
- [ ] Local lights continue working.

## 3. Options considered

**A. Bounding-sphere roof map.** Rasterise instance bounding spheres into a coarse 2D
"roof height" grid; a fragment under a roof loses sky ambient. *Rejected.* The renderer only has
bounding SPHERES (`WgrInstance::center.w` × `model.bounding_sphere`), never boxes. A sphere around
a 20 m building extends metres past its walls, so the street outside would darken. Cheap, and
visibly wrong.

**B. Top-down orthographic depth map (this plan, Stage 1).** Render the retained set from
directly above the camera into a depth target. A fragment with geometry above it is indoors.
Geometrically exact for the overhead case, and there is a working template: the planar reflection
already runs an entirely separate cull view + args + pass (`set_reflection_params`,
`cull_dispatch_reflection`), so this is a second instance of a proven pattern rather than new
machinery.

**C. Voxelisation + outside flood fill.** Voxelise geometry, flood from outside, mark reachable
air. The general answer — handles windows, doors and porches uniformly. Needs a voxelisation pass
the engine has no geometry feed for, plus an iterative fill and a volume texture. This is the
Stage 2/3 target, not a first step.

**D. Portals.** Author or derive window/door polygons. No authoring exists in this data set and
deriving them from 2001 P3Ds is its own research project.

**E. Per-model baked sky-visibility volume (adopted as Stage 2, 2026-08-05).** Bake the dome
integral once per MODEL, at load time, into a small model-space 3D texture; sample it per fragment.
See §3c — this replaces the per-frame tilted maps that §3b originally proposed.

**F. Hardware ray tracing.** *Rejected, and not a close call.* `wgpu-types 29.0.4` gates ray queries
behind `EXPERIMENTAL_RAY_QUERY`, whose own doc comment reads "Supported platforms: Vulkan" and
"expected to be subject to breaking changes" (`features.rs:1042-1053`). This renderer comes up on
DX12 on Windows, and GL33 is still the supported fallback backend. Putting the one feature that
makes interiors work behind a Vulkan-only experimental flag means it does not exist for most users.
A software BVH over the resident bindless mesh data is possible but is a multi-week project for a
term that only modulates ambient.

### 3a. Why NOT the sun direction (asked 2026-08-05, and worth recording)

The natural suggestion is to occlude along the SUN vector rather than straight down. It is the
wrong light, and it fails in a way that is easy to miss:

- The thing making interiors flat is the **sky ambient** — the SH projection of the whole dome,
  arriving from every direction at once. It is not the sun.
- **Direct sun is already occluded**, by the cascade shadow maps plus the terrain sun-shadow mask.
  A room under a roof already receives no direct sun. That was never the gap.
- Testing along the sun vector inverts at low sun: at sunset the sun is near horizontal, so a room
  with a west-facing window would read "fully lit" while an open courtyard read "dark". The test
  direction has nothing to do with where the sky is.

The zenith is the right FIRST direction because a cosine-weighted hemisphere is dominated by it.

### 3b. The good part of that question: sample the dome, not one direction

The honest objection to a single top-down map is that it is one direction and the sky is a dome.
Render the depth map from SEVERAL directions — zenith plus a few tilted toward the horizon — and
take the unoccluded fraction, and the result actually approximates sky visibility.

This matters more than it first appears: **a tilted map can see through a window**, because its
rays arrive near-horizontally. That is precisely the "window-adjacent room receives light"
criterion, which a zenith-only map structurally cannot satisfy and which was going to be deferred
to voxels or portals. Same machinery, N views instead of one.

Directions must be FIXED in world space, not rotated with the camera or the time of day — a moving
sample set is a temporal artifact generator, and there is no TAA here to hide one.

The original form of this section proposed paying for that dome sampling **per frame**: 5 depth
passes (zenith + 4 tilted ~50 deg) over the retained set, every frame. §3c is why that is the wrong
place to spend it.

### 3c. Sample the dome per MODEL, not per frame (correction, 2026-08-05)

§3b is right that the window criterion needs tilted directions. But a per-frame map is sized by a
world box around the camera, and that resolution does not reach:

| | box | at 1024² | window reveal ~1 m, wall ~0.3 m |
|---|---|---|---|
| per-frame map | 256 m | **25 cm/texel** | cannot separate window from wall |
| per-model bake | one 20 m building | **2 cm/texel** | resolves reveal, frame and wall thickness |

At 25 cm/texel the tilted map does not merely fail the window criterion — it fails it
*stochastically*, leaking sky through walls at some building orientations and sealing windows at
others. And with 4 azimuths, whether a given building's windows work at all depends on how that
building happens to be rotated in the mission.

#### 3c-bis. …and why the tilted maps were built anyway (correction, 2026-08-05, same day)

The table above is right about the numbers and wrong about the conclusion, because **the box size
is a free variable and I took it as given**. 25 cm/texel comes from a 256 m box. Shrink the box to
what the feature actually needs — it only has to cover the buildings you are standing among, not
the view distance — and at 64 m half-extent with 2048 texels it is **6 cm/texel**, which resolves
a 1.5 m window reveal across ~25 texels.

Smoke testing settled it. With the zenith map alone, a real building read exactly as this section
predicted a zenith map must: the room was uniformly darker, and the only bright surfaces were the
outer walls — not window light, but the softening kernel's taps escaping past the building's
footprint where there is no roof. That is a wrong-looking artifact, not a partial success.

So Stage 1 ships five directions. The bake is still the better answer (it sees a window at 2 cm
from any angle, costs nothing per frame, and is temporally exact), but it is no longer the ONLY
answer, and shipping the cheap one first is what put a real building in front of a real tester.

The occluder that decides whether a point in a room sees sky is, almost always, **the building that
room is in**: one rigid model, from a set of a few hundred, whose shape never changes. So run §3b's
machinery once per model at load time instead of once per frame:

- ~64 directions over the hemisphere (affordable when amortised per model), cosine-weighted
  unoccluded fraction accumulated into a model-space 3D texture. 32×32×16 `R8Unorm` ≈ 16 KB per
  model; ~500 models ≈ 8 MB.
- Sampling needs no search. `gpu_driven.wgsl`'s `Instance` already carries `model: u32`
  ([gpu_driven.wgsl:34](../rust/src/gfx3d/gpu_driven.wgsl#L34)) and the vertex `pos` is model space
  ([:107](../rust/src/gfx3d/gpu_driven.wgsl#L107)). A fragment on a building's own wall or floor
  samples its own model's volume — one trilinear tap, indexed by what it is already drawing.
- Trilinear filtering replaces the world-space kernel of §4, and because the volume is model space
  the result is **temporally exact**: no ortho snapping, no crawl, nothing for the GTAO mip-march
  failure mode to repeat in. Instance rotation is handled for free.

What the bake does not cover, and what therefore keeps the zenith map alive: terrain under a
building (the ground is not part of the model), units and objects standing in a room (their model is
not the occluder), and composite occlusion between two separate buildings. The first two want a
short "which nearby building volumes contain me" loop — ~32 entries, against the 256-entry flat
light loop already running per fragment, that is noise, and it does not wait on Forward+. The third
is exactly what a single zenith map is good at.

Known limits, stated up front: the bake is the base pose, so animated doors bake as authored; and
destroyed building states are separate models, so they bake separately and correctly.

This structure is also what `WTR-250` rain wants for "is this point under cover" — build it once.

### 3d. Stage 2 constraints (review, 2026-08-05) — settle these BEFORE building the bake

A second review raised five objections. Four are accepted as constraints on Stage 2; one needed its
arithmetic corrected first.

1. **Bake cost is real, but not for the stated reason.** "64 × 1024² = 67M depth samples per model"
   counts fragments, not work: this is 64 rasterisations of one low-poly 2001 building, and fill
   rate is the only meaningful term. At ~10 GPix/s that is ~7 ms per model — but ~500 models is
   still **~3.5 s of load stall**, plus tens of thousands of pass submissions. So the conclusion
   holds even though the framing does not: the bake must be **cached on disk, keyed by a content
   hash of the model**, with a version field, and it must run in the background with a safe
   fallback (reach = 1, i.e. no darkening) while a volume is missing. Bring up on ~5 representative
   buildings, not the whole library.
2. **8 MB is the raw-payload figure, not the footprint.** Atlas padding, alignment, bounds/transform
   metadata and any index structure are not in it. Measure before quoting it.
3. **Do not loop 32 volumes per fragment.** Resolve the containing building **per object**, not per
   pixel: the fragment then does one sample with an index handed to it. The per-fragment loop was
   priced against the 256-light loop, which is itself a known cost to be removed by Forward+ — the
   wrong thing to benchmark against.
4. **State the model-variance policy explicitly**, do not discover it: animated doors, damaged and
   destroyed models, alpha-tested/glass windows, proxies and separately placed building parts,
   visual LODs, and **mirrored or negatively scaled instances** (a model-space volume is orientation
   free but is NOT mirror free). Each needs an answer in the Stage 2 plan, and the cache needs an
   invalidation policy that covers them.
5. **Self-occlusion is not the world.** One object roofing another, two structures forming a covered
   space, bridges, modular pieces, terrain overhangs, mission-placed geometry — none are visible to
   a per-model volume. This is agreement with the staging above, not an objection to it: Stage 1's
   world-space zenith map stays as the broad fallback and the two composite.

## 4. Stage 1 design (option B, zenith only)

Stage 1 ships **one** map, straight down. The tilted directions are not built here: §3c gets that
answer from the bake instead, at a resolution where it actually works. What Stage 1 owns is the
plumbing every later method needs — the extra cull view, the depth target, the ambient attenuation
point, the floor, and the debug view.

- One depth-only target, `Depth32Float`, covering a world box around the camera —
  start ~256 m, snapped to texel size so it does not shimmer as the camera moves. **Snapping is
  required, not polish**: an unsnapped ortho view resamples every frame and the resulting
  crawl is exactly the class of artifact the GTAO mip march was reverted for (no TAA to hide it).
- One extra cull view with the ortho VP; the existing per-view cull produces the args. The planar
  reflection is the template for a standalone extra view.
- Reuse `gpu_driven_shadow.wgsl`'s depth-only draw with the sky VP supplied via a pass UBO.
- Bind at `frame @binding(12)` + a `sky_reach(world_pos)` helper.
- In the ambient term: attenuate the SH sky irradiance when geometry sits above, toward a floor.

**Terrain is deliberately excluded from the map.** Terrain is never "above" anything the player
stands on, and including it would make every hillside a roof.

### Softening — the part that decides whether it looks right

A hard test ("is anything above me") gives black rooms and a hard line at the doorway. Sample the
map over a KERNEL in world space and take the fraction of taps that are unoccluded: under the
middle of a roof every tap is blocked, at the edge some see sky, so a porch grades. Kernel width
is the tuning knob and roughly sets how far light appears to reach in from an opening.

This is what gets "porch partly dark" without portals.

### Expected result against acceptance

| criterion | Stage 1: zenith map | Stage 2: + per-model bake (§3c) |
|---|---|---|
| Porch partly dark | yes (kernel softening) | yes, and correctly shaped |
| Window-adjacent room receives light | **no** — structurally impossible | yes — 2 cm texels resolve the reveal |
| Deep room is dark | yes | yes |
| Sealed bunker no skylight | yes | yes |
| Local lights continue working | yes (ambient term only; direct + local untouched) | yes |

The window row is the one criterion Stage 1 cannot reach, and it is the reason Stage 2 exists.
Measure it on a real building before claiming it either way.

### Stage 1 as built (2026-08-05)

Implemented as described: `gfx3d/sky_vis.rs` owns the snapped ortho view, `CullState`'s `sky_view`
is the extra cull view (the reflection view's twin — both now share `prepare_standalone_view`), the
map is drawn by the EXISTING GPU-driven shadow depth pipeline through a reserved shadow-pass-UBO
slot (`SKY_UBO_SLOT`), and `frame @binding(12)` + `interior_sky_reach` / `interior_sky_ao` apply it
to the ambient of objects AND terrain. Knobs ride `WgrRenderParams`; C++ has an `Interior Sky` tab
and `WGR_INTERIOR_SKY` / `_DEBUG` / `_EXTENT`.

**Measured, not assumed.** The renderer logs a one-shot coverage report — the fraction of map texels
holding an occluder — because an empty map is INDISTINGUISHABLE from a working one in every other
signal: it clears to the far plane, every depth comparison passes, reach is 1, no validation fires.
The first live run reported `0.00%`, and the sequence that followed is worth recording:

| step | result | what it ruled out |
|---|---|---|
| coverage 0.00%, args/bind present | inert | resource setup |
| sub-draw count from the args buffer | 0 | not the draw — the cull kept nothing |
| "skip the frustum" experiment | still 0 | **no-op**: `set_sky_params` OVERWRITES `debug_flags` |
| same experiment, applied where it lands | 192 sub-draws | the frustum was the rejector |
| planes logged live | correct, ±128 m around the camera | the planes are right |
| box widened to 4 km (`WGR_INTERIOR_SKY_EXTENT`) | 44 sub-draws, **0.55% coverage** | nothing was wrong |

The 128 m box was empty because the dev-map camera stands where the retained set has nothing within
128 m — the main view had ONE sub-draw at the same moment. So the chain is verified working end to
end, and the lesson is the plan's own: the first "falsification" was a no-op that agreed with the
bug, which is exactly the failure mode the handover warns about.

### Measured cost (2026-08-05, first real building, 800x600 windowed, RWDI)

| region | ms |
|---|---|
| interior sky: cull (5 direction chains) | 0.527 |
| interior sky: depth maps (5 passes) | 0.260 |
| **feature total** | **0.787** |
| GPU frame total | 7.123 |
| share of frame | **11.1%** |

A second measurement, on an area where nothing is in the box, separates the two terms much more
sharply: `isky_cull=0.362 isky_maps=0.055` of a 6.47 ms frame. The DEPTH MAPS are nearly free when
there is nothing to draw — but the CULL is not, because it sweeps the whole instance table whether
or not anything survives. So roughly 0.36 ms is a FIXED tax paid per frame regardless of scene
content, and it is ~7x the cost of the rasterisation it feeds.

The surprise is that the CULL costs twice the DRAW. Each of the five views runs the full
three-pass collapse over the whole retained instance table (~98k instances) to keep a handful that
fall inside a 64 m box. The draw is cheap precisely because so little survives.

That points at the fix: the five boxes cover nearly the same neighbourhood, so one shared cull over
a conservative union — with the TILTED views' along-axis depth cut right down (they only need to
reach through an opening a few metres away, not 300 m) — would replace five instance sweeps with
one. Not done yet; measure it, do not assume it.

### Stage 2 progress (2026-08-05): the bake core exists and is proven

`gfx3d/sky_bake.rs` + `sky_bake.wgsl` + `sky_bake_depth.wgsl` implement the bake itself: rasterise
one model into a depth map per sky direction IN MODEL SPACE, then reduce those maps into a
model-space visibility volume. Pure and synchronous by design — the caching and scheduling §3d
demands live above it, so the core stays testable.

Proven on a synthetic room (6 x 3 x 6 m, walls + floor + ceiling, 25 directions: zenith plus rings
at 35 and 65 deg):

| assertion | result |
|---|---|
| sealed room interior | < 0.15 visibility |
| above the roof | > 0.7 |
| window side vs far corner (same room, hole in the +X wall) | window side higher by > 0.05 |
| **same comparison with the hole FILLED** | gradient absent — so it is the window, not the shape |

That third row is the falsifier built into the suite: without it, "the window side is brighter"
would pass on any volume that merely darkens toward corners. All three fail when the depth test is
seeded out of the reduce shader, checked rather than assumed.

**Now fed by real geometry (2026-08-05).** `register_model` bakes LOD 0 straight out of the shared
geometry pool — no CPU copy of the mesh, no second source of truth for a model's geometry — using
the model-space AABB each mesh computed once at `mesh_create`, while its vertices were still on the
CPU. 41 directions (zenith + rings at 30/55/75 deg), 32x16x32 voxels, 1024² maps. Gated behind
`WGR_SKY_BAKE_VOLUMES=1`, off by default, because §3d's caching does not exist yet.

Measured on real OFP models, first run:

| model | bbox | enclosed (vis < 0.5) |
|---|---|---|
| house | 9.6 x 12.4 x 9.1 m | 24.9% |
| large building | 30.9 x 10.4 x 34.0 m | 47.8% |
| small prop | 1.8 x 1.3 x 1.8 m | 20.9% |

The correlation is the sanity check: more interior volume, more enclosed voxels. **~18-22 ms per
model**, which is §3d's predicted load stall arriving exactly on schedule — ~500 models is ~10 s,
so the content-hashed cache is a requirement and not a nicety.

**Still to do before this replaces the per-frame path:** a volume atlas plus per-model offset/scale
in the model table, the per-fragment sample in `gpu_driven.wgsl`, and everything in §3d — content-hashed disk cache, background scheduling with a
reach = 1 fallback, per-object volume lookup for movers, and the model-variance policy.

## 5. Risks

- **Temporal stability.** Snap the ortho origin to texel size. See the GTAO mip march for what
  happens when a screen-space/level quantity is allowed to vary continuously with the camera.
- **Too dark.** OFP interiors carry few local lights. A floor is required, and the honest default
  is probably a fairly generous one until someone walks a mission.
- **Cost.** One extra depth pass over the retained set, plus a cull view. Measure it; the AO work
  went in unmeasured and that should not repeat.
- **Doorways popping** as the camera crosses the map's edge — the box must extend well past the
  view distance that matters, or fade out at its border. (Built: the reach fades over the outer 5%
  of the map instead of ending in a line that would sweep across the world as the player walks.)
- **A tilted map's usable extent is bounded by its height.** Looking along a ~50 deg slant, a
  point `extent` away laterally sits `extent * sin(50)` along the view axis, so once that exceeds
  `height` it leaves the depth slab and the tilted maps stop contributing — while the zenith map
  carries on working. The symptom is subtle: roofs still darken, window light quietly stops.
  Measured on the way in (extent 1500 / height 300 left two of four tilted layers empty). The
  defaults sit far inside the limit (64 * 0.77 = 49 m against 300 m of height) and the dev tab
  warns when a hand-set pair crosses it.
- **Banding from too few kernel taps** (found in smoke testing, fixed). A single ring of four taps
  gives a direction only six possible values; at a kernel wide enough to soften a doorway (1 m =
  16 texels at the default) those six levels read as hard STEPPED PATCHES across a ceiling, which
  looks like wrong geometry rather than a sampling artifact. Now two rings, nine taps, inner ring
  rotated 45 deg.
- **Grazing-angle self-shadowing** (same session, fixed). A fixed depth bias is only correct for a
  map looking straight at the surface; a tilted direction meeting a wall at a grazing angle spans
  far more depth per texel, and a head-on bias leaves that wall striping itself. The bias is now
  slope-scaled by 1/cos of the surface-to-direction angle, clamped so a near-edge-on surface cannot
  demand an unbounded bias and start leaking.
- **Vegetation is in the map.** Nothing excludes it — the cull has no per-instance vegetation bit,
  and adding one is a per-section property the instance does not carry. So a forest canopy reads as
  a roof. That is arguably correct occlusion, but it is untested against the look and it is the
  most likely source of a "why is the whole wood dark" report. Judge it from the reach buffer
  first; a canopy that reads too strong is a reason to add the filter, not to lower the strength.

## 6. Stages

1. **Five-direction depth maps + kernel-softened, DIRECTION-STEERED sky attenuation.** Zenith plus
   four tilted at ~50 deg, 6 cm/texel; the ambient is steered toward the direction the sky actually
   reaches a point from, not merely scaled by how much of it does. Debug view of the reach factor,
   GPU-timer rows, default OFF, ImGui tab. **This plan.** *(Shipped as one zenith map first; §3c-bis
   records why that was not enough and what smoke testing showed.)*
2. **Per-model baked sky-visibility volume** (§3c, option E). Owns the window criterion, and
   subsumes most of what per-frame tilted maps were for. Composites with Stage 1's map, which keeps
   the cases the bake structurally cannot see (terrain, inter-building, movers).
3. Polish: reach-modulated local-light falloff, coupling to the bent normal so interior ambient
   arrives from the opening rather than from straight up.
