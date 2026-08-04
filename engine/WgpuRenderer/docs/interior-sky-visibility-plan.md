# Interior sky visibility (LIT-020) — plan

**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust). **Status:** PLAN (2026-08-05).
**Roadmap:** `LIT-020 — Geometry-aware interior sky visibility — REQUIRED outcome`.

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

Cost is N depth passes over the retained set. Start at 5 (zenith + 4 tilted ~50 deg, evenly spaced
in azimuth); the tilted ones are a correction term and can be lower resolution than the zenith map.
Weight each by its cosine so the zenith still dominates, matching the diffuse response.

Directions must be FIXED in world space, not rotated with the camera or the time of day — a moving
sample set is a temporal artifact generator, and there is no TAA here to hide one.

## 4. Stage 1 design (option B)

- N depth-only targets (see §3b; start with zenith + 4 tilted), `Depth32Float`, covering a world
  box around the camera —
  start ~256 m, snapped to texel size so it does not shimmer as the camera moves. **Snapping is
  required, not polish**: an unsnapped ortho view resamples every frame and the resulting
  crawl is exactly the class of artifact the GTAO mip march was reverted for (no TAA to hide it).
- One extra cull view per direction, each with its ortho VP; the existing per-view cull produces
  the args. The planar reflection is the template for a standalone extra view.
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

| criterion | zenith map only | + tilted directions (§3b) |
|---|---|---|
| Porch partly dark | yes (kernel softening) | yes |
| Window-adjacent room receives light | **no** — structurally impossible | **plausibly yes** — a tilted ray enters the window |
| Deep room is dark | yes | yes (no direction reaches it) |
| Sealed bunker no skylight | yes | yes |
| Local lights continue working | yes (ambient term only; direct + local untouched) | yes |

The window row is the reason to do §3b rather than ship the zenith map alone. It is marked
"plausibly" because it depends on a tilted direction actually clearing the window reveal and the
room's depth — measure it on a real building before claiming the criterion.

## 5. Risks

- **Temporal stability.** Snap the ortho origin to texel size. See the GTAO mip march for what
  happens when a screen-space/level quantity is allowed to vary continuously with the camera.
- **Too dark.** OFP interiors carry few local lights. A floor is required, and the honest default
  is probably a fairly generous one until someone walks a mission.
- **Cost.** One extra depth pass over the retained set, plus a cull view. Measure it; the AO work
  went in unmeasured and that should not repeat.
- **Doorways popping** as the camera crosses the map's edge — the box must extend well past the
  view distance that matters, or fade out at its border.

## 6. Stages

1. **Overhead depth map + kernel-softened sky attenuation.** Four of five acceptance criteria.
   Debug view of the reach factor, default OFF, ImGui tab. **This plan.**
2. **Lateral reach**, only if the tilted directions prove insufficient on real geometry — voxel
   fill (option C) or portals (option D). Decide between them only after Stage 1 has been seen,
   because Stage 1 determines how much of the problem is left.
3. Polish: reach-modulated local-light falloff, coupling to the bent normal so interior ambient
   arrives from the opening rather than from straight up.
