# Grass Plan for the CWR-CE WGPU Renderer

Reviewed against the `new-renderer-infrastructure` checkout and the local
`inspiration/GodotGrass-main` reference on 2026-07-26.

## Verdict

GodotGrass is a strong visual reference, but it is not an implementation that
should be ported literally.

Port:

- The five-segment curved blade silhouette.
- Base-to-tip colour, root darkening, subtle per-blade variation, and clumping.
- Height-dependent wind, macro gusts, and tip turbulence.
- Bent blade normals, soft dark-side lighting, and backlighting/transmission.
- A clamped minimum projected blade width.
- The simple local crushing idea as an interaction prototype.

Do not port:

- Godot nodes or `MultiMeshInstance3D`.
- The CPU-generated random transforms.
- The 5 m camera-relative tile relocation.
- The hard density and mesh changes at 12/40/70/100 m.
- The reference heightmap coordinate assumptions.
- Its custom lighting as a separate lighting system.

The reference itself documents its visible tile LOD changes. Its `main.gd`
also regenerates positions with unseeded `randf_range`, then physically moves
the tile set when the camera crosses a tile boundary. That is acceptable for a
demo and unsuitable for a persistent battlefield.

The target is:

> Deterministic world-space procedural blades, selected and compacted on the
> GPU, attached to the renderer's existing terrain heightmap, drawn indirectly
> through the existing depth, colour, shadow, fog, and water ordering.

## Source-grounded constraints in this checkout

The implementation must fit what exists now, rather than what older planning
documents intended:

- Terrain already owns a persistent `R32Float` heightmap and exact
  triangle-interpolated `sample_height`, matching `Landscape::SurfaceY`.
- Terrain already owns a per-land-cell integer index map, but its texture view
  is not currently exposed outside `Terrain`.
- `Terrain::heightmap_view`, generation tracking, and conform parameters are
  already lent to object rendering. Grass should follow this internal resource
  sharing pattern.
- The renderer already has a unified terrain/object depth+normal prepass,
  builds Hi-Z between prepass and colour, then renders the colour pass.
- Water is not replayed at its command position. It is deliberately deferred
  to a dedicated pass after the opaque colour pass, where it samples a frozen
  scene colour and read-only resolved depth.
- Cascaded shadows run before the main scene passes. Grass shadows therefore
  must join the existing cascade target and ordering; a disconnected shadow
  system is not acceptable.
- MSAA and alpha-to-coverage already exist for cutout foliage. Grass should use
  the same sample-count policy.
- GPU-driven object culling, indirect draws, Hi-Z, timers, terrain shadow,
  sky visibility, foliage transmission, and aerial fog are implemented.
- `Landscape::GetGeography` already provides per-cell road, track, water,
  forest, obstacle, and gradient bits. This is the best first exclusion and
  ecology source.
- `Landscape::GetWind()` already provides the live gameplay wind and gust
  vector. Grass should use it from the first wind implementation.

## Important changes from the earlier plan

### 1. Start stateless; do not start with a persistent clipmap

The proposed rings contain only roughly 100k-150k candidates:

- Near: 0-25 m at about 0.25 m spacing.
- Mid: 25-60 m at about 0.5 m spacing.
- Far: 60-120 m at about 1.0 m spacing.

A small compute pass can reconstruct, filter, and compact that candidate set
every frame. This has several advantages:

- No scrolling ring state.
- No newly exposed row/column bookkeeping.
- Teleports require no special resource migration.
- No stale cells, duplicates, or wraparound bugs.
- Camera rotation cannot affect placement.
- Preset changes take effect immediately.

The camera origin is snapped independently for each ring. Candidate identity is
derived only from integer world-cell coordinates, a map seed, and a slot ID.
Grass therefore does not slide when the camera moves.

Incremental clipmap caching is an optional later optimisation. It should only
be implemented if GPU timing proves that stateless compaction is material.
"Never regenerate all grass" is not a valid invariant by itself.

### 2. Do not require clusters for the first version

At this candidate count, per-candidate frustum, distance, terrain, and ecology
tests are simple and predictable. Start with direct candidate compaction.

Add 4x4 m or 8x8 m cluster culling only if profiling shows that candidate
testing, Hi-Z sampling, or shadow culling needs it. Do not build both a cluster
cache and a blade cache before there is evidence that either is necessary.

### 3. Use ordinary indirect draws, not the general object multi-draw path

Grass has one procedural geometry family and does not need the object
renderer's merged mesh pool or bindless material grouping.

Use three compact visible-instance lists and three `DrawIndirectArgs` records:

- Near segmented blades.
- Mid simplified blades.
- Far triangles.

Issue one `draw_indirect` per non-empty LOD. This avoids dependence on
`INDIRECT_FIRST_INSTANCE` and `MULTI_DRAW_INDIRECT_COUNT`, and remains portable
across the renderer's supported native backends.

### 4. Join the existing passes

Do not create a parallel frame graph with "opaque depth", "grass depth",
"opaque colour", and "grass colour" as unrelated passes.

Grass should be inserted into:

1. The existing cascade shadow target, when enabled.
2. The existing depth+normal prepass for near/mid blades.
3. The existing opaque colour pass.
4. The existing scene snapshot seen by water, automatically because grass
   colour is complete before the water pass.

Initially exclude grass from planar reflections.

### 5. Use the existing geography map before inventing a stamping system

Upload a compact integer geography texture when a landscape loads. Preserve the
existing `GeographyInfo` bits and use them to reject or modify candidates:

- Reject water cells and off-map cells.
- Reject roads and tracks initially.
- Reject or strongly reduce `full` and hard-obstacle cells.
- Reduce density in dense forest cells.
- Use the gradient bit as a cheap coarse guard, followed by the exact
  heightmap-derived slope test.

This immediately handles much of the world with existing authoritative data.
Fine road edges, exact building footprints, runways, and mission-authored clear
areas can be added as a second, higher-resolution override mask later.

## Renderer architecture

### Rust module

Recommended layout:

```text
engine/WgpuRenderer/rust/src/grass/
|-- mod.rs
|-- place.wgsl
|-- geometry.wgsl
|-- render.wgsl
|-- shadow.wgsl
`-- interaction.wgsl        # added later
```

`geometry.wgsl` must contain the shared candidate decoding, terrain attachment,
blade curve, wind, LOD coverage, and interaction deformation used by colour,
prepass, and shadow entry points. Duplicating these formulas will create
detached shadows and depth silhouettes.

Initial ownership:

```rust
pub struct GrassSystem {
    params_buffer: wgpu::Buffer,
    species_buffer: wgpu::Buffer,

    geography_texture: wgpu::Texture,
    geography_view: wgpu::TextureView,
    ecology_texture: wgpu::Texture,
    ecology_view: wgpu::TextureView,

    visible_near: wgpu::Buffer,
    visible_mid: wgpu::Buffer,
    visible_far: wgpu::Buffer,
    indirect_args: wgpu::Buffer,
    counters: wgpu::Buffer,

    place_pipeline: wgpu::ComputePipeline,
    color_pipelines: [wgpu::RenderPipeline; 3],
    prepass_pipelines: [wgpu::RenderPipeline; 2],
    shadow_pipelines: [wgpu::RenderPipeline; 2],

    interaction: Option<GrassInteraction>,
}
```

Do not store a second heightmap. During resource preparation, `Renderer` lends
the current terrain heightmap view, exact sampling parameters, and generation
number to `GrassSystem`, just as it already does for terrain-conformed objects.
Expose a persistent `index_map_view` and a distinct generation counter only if
the grass ecology pass actually needs the raw terrain layer index.

### C ABI and command stream

Add:

```cpp
WGR_CMD_DRAW_GRASS = 6
```

and:

```cpp
struct WgrGrassBatch
{
    uint32_t camera;
    uint32_t flags;
    uint32_t _pad0;
    uint32_t _pad1;
};
```

Add a `grass_batches` slice to `WgrFrame` and mirror every layout change in C++
and Rust with size assertions.

`TerrainWgpu::DrawTerrain` is already the authoritative map/camera point. After
submitting its terrain batch, it should submit one grass batch for the same
main-world camera when grass is enabled. No new generic engine rendering
interface is required for the first version.

The command becomes `Plan3dOp::Grass(arg)`. It is:

- Replayed in the unified prepass.
- Replayed in the unified colour pass.
- Skipped by the 2D and water-only passes.
- Not replayed for planar reflections during initial development.

Grass shadow preparation happens before the cascade render, using the main
grass batch camera and the existing `WgrShadowPass`.

### Persistent configuration

Use one versioned, consolidated structure rather than many setters:

```cpp
struct WgrGrassParams
{
    uint32_t enabled;
    uint32_t quality;
    uint32_t debug_mode;
    uint32_t flags;

    float draw_distance;
    float shadow_distance;
    float density_scale;
    float max_slope;

    float near_end;
    float mid_end;
    float lod_fade_width;
    float water_margin;

    float height_scale;
    float width_scale;
    float root_ao;
    float transmission;

    WgrVec4 wind; // xyz = live world-space velocity, w = turbulence scale
};
```

Static geography/ecology maps are uploaded on landscape change. Live params,
time, wind, and interactors update without reallocating those textures.

## Deterministic placement and compaction

Use a canonical finest world grid, initially 0.25 m. Coarser rings process
stable subsets of that grid:

- Near stride 1.
- Mid stride 2.
- Far stride 4.

Only integer world coordinates and stable hash thresholds choose survivors.
The same candidate that survives into a coarser ring keeps its position and
seed.

Candidate identity:

```text
hash(map_seed, base_cell_x, base_cell_z, candidate_slot)
```

The hash selects:

- Jitter within the canonical base cell.
- Orientation.
- Height and width.
- Lean and curvature.
- Colour/temperature variation.
- Species.
- Clump response.
- Stable density threshold.

The compute pass:

1. Resets counters and indirect instance counts.
2. Enumerates the snapped near/mid/far ring grids.
3. Rejects the inner square for coarser rings and rejects outside the circular
   draw range.
4. Applies frustum and optional horizon checks.
5. Rejects outside terrain bounds.
6. Samples exact terrain height and reconstructed normal.
7. Applies water, slope, geography, and ecology rules.
8. Applies stable density thinning.
9. Appends a compact record to the relevant visible list.
10. Writes the three indirect instance counts.

A suitable 16-byte record is:

```rust
struct GrassInstance {
    cell_x: i32,
    cell_z: i32,
    seed_species: u32,
    packed_shape_color: u32,
}
```

World Y is not stored. Every rendering pass samples the shared heightmap so
roots remain exact after a terrain resource replacement.

Capacities are derived from the preset's enumerated candidate maximum, with a
small safety margin. Overflow sets a visible diagnostic flag; it never silently
truncates without reporting.

## Terrain ecology

Implement ecology in layers.

### Stage A: authoritative coarse filters

Upload `Landscape::GetGeography(x,z).packed` as a world-aligned integer texture.
Use water, road, track, forest, obstacle, and gradient bits.

Also apply exact shader tests:

- `sample_height` with the terrain's triangle split.
- `sample_normal` with the terrain grid step.
- `normal.y` slope threshold.
- Animated sea level plus a shoreline margin.
- Strict map bounds, even though the terrain renderer visually extends edge
  heights beyond the island for the ocean seabed.

Grass must not use the terrain renderer's extended off-map border.

### Stage B: surface/ecotype map

The current index map identifies a rendered texture layer, but pre-blended
transition textures can represent multiple underlying surfaces. Do not assume
that one index always equals one ecology.

Build a small C++ ecology map from the landscape's resolved `SurfaceInfo`
quadrants (or sample `Landscape::SurfaceAt` at quadrant centres). Each texel
stores density, species group, colour family, and flags. Provide a conservative
default so old islands work without authoring.

Rules should be configuration-driven and keyed by surface name or category:

- Meadow: dense mixed grass.
- Dry ground: sparse, shorter, warmer grass.
- Forest floor: sparse shade grass.
- Sand/shore: none initially; reeds as an explicit later species.
- Rock/concrete: none.
- Mud: sparse dark grass.

### Stage C: fine exclusion overrides

Only after Stage A/B works, add a higher-resolution optional mask for:

- Exact road and runway widths.
- Building footprints.
- Concrete pads.
- Large rock footprints.
- Mission-defined clear zones.
- Mown or agricultural areas.

This mask refines the coarse geography map; it does not replace it.

## Blade geometry

Generate triangle vertices from `vertex_index`; do not upload the reference OBJ.

- Near: five vertical segments plus a pointed tip, matching the useful shape of
  `grass_high.obj`.
- Mid: two segments plus a pointed tip.
- Far: one triangle.

Suggested profile:

```text
t            = height coordinate [0,1]
half_width   = width * (1 - t)^width_falloff
static_bend  = lean * t^2
wind_bend    = wind * smoothstep(root_lock, 1, t)^2
tip_curve    = curvature * sin(t * pi/2)
```

Keep the lower 20-30% nearly fixed.

For minimum apparent width, offset along the projected blade-side direction in
clip/NDC space and clamp the correction to a small pixel range. Do not reproduce
the reference shader's per-vertex inverse model-view reconstruction or its
unbounded distance widening. The goal is about one-pixel continuity, not wide
distant ribbons.

Render double-sided initially (`cull_mode: None`). If double-sided cost is
material, profile crossed-card or orientation strategies later.

## LOD and coverage

No alpha-blended grass field.

Use:

- A stable world-hash survival threshold for density.
- Nested candidate subsets between rings.
- A transition band with complementary stable coverage.
- Alpha-to-coverage when MSAA is active.
- A stable dither discard at 1x.
- The exact same coverage function in prepass, colour, and shadow.

Far grass should converge toward terrain colour and aerial fog. Do not preserve
high-contrast individual green lines to the maximum range.

FOV-aware projected size may refine LOD later, but world distance is adequate
for the first visual milestone.

## Wind and interaction

### Wind

Feed `Landscape::GetWind()` every frame. Build motion from:

1. The live world-space wind vector.
2. Low-frequency world-space macro gust noise.
3. Higher-frequency turbulence.
4. Stable per-blade phase variation.

Use analytic hash/value noise first to avoid adding copied noise assets. A small
generated noise texture is acceptable if profiling shows it is better.

All coordinates and time are world/simulation based. Colour, prepass, and
shadow entry points call the same deformation function.

### Interaction Stage A

Add a bounded array of analytic interactors:

```cpp
struct WgrGrassInteractor
{
    WgrVec4 position_radius;
    WgrVec4 velocity_strength;
    WgrVec4 shape_recovery_type_flags;
};
```

Start with player/character capsules and vehicle spheres or capsules. Bend away
from the closest few interactors; keep the root anchored.

### Interaction Stage B

Only after analytic interaction is stable, add a camera-centred persistent bend
field:

```text
RG = horizontal bend
B  = flattening
A  = age/recovery
```

Use compute re-projection, decay, and stamping. Keep it visual-only with no CPU
readback.

## Lighting and fog

Grass uses the existing frame camera, sun, local lights, cascade shadow,
terrain shadow, sky visibility, sky irradiance, and aerial fog resources.

It should have a dedicated thin-blade shading function rather than blindly
calling the tree-canopy branch:

- Curved blade geometric normal with an upward bias.
- Terrain-normal blend near the root.
- Wrapped diffuse to avoid black back faces.
- Backlighting/transmission gated by sun visibility.
- Root AO based on blade height and local density.
- Subtle base-tip, species, clump, wet/dry, and per-blade colour variation.
- Reduced specular and high roughness.
- Existing aerial fog/froxel sampling.

Reuse shared shadow, local-light, sky, and fog helpers. Extract a common thin
transmission helper from foliage shading if useful, but keep grass-specific
parameters and normals.

## Depth and shadows

### Depth prepass

Near and mid blades write depth and view-space normals into the existing
prepass targets. Far triangles initially skip the prepass.

The prepass uses the exact same:

- Candidate record.
- Terrain attachment.
- Wind deformation.
- Interaction deformation.
- Projected-width correction.
- LOD coverage.

After this, the existing Hi-Z build includes near/mid grass. Optional
current-frame Hi-Z colour re-culling can be added later; the first version only
needs the pre-placement frustum/distance/ecology compaction.

### Cascaded shadows

Add shadows after the basic colour system is stable.

- Near grass only at first.
- Independent grass shadow distance, initially 25-40 m.
- Reduced stable density.
- Simplified two-segment silhouette.
- Same world seed, wind, and interaction deformation.
- Cull against each active cascade; never submit all candidates to every layer.
- Render into the existing cascade depth array before scene colour samples it.

Do not allocate a second cascade map for grass.

## Delivery phases

### GRASS-000 - Architecture contract and baseline

Document and verify:

- Exact `WgrFrame`/command changes.
- Terrain heightmap and prospective geography/ecology ownership.
- Existing prepass, Hi-Z, colour, water, resolve, planar, and shadow hooks.
- Buffer formats, capacities, and bind groups.
- Fixed-camera screenshots and GPU timing baseline.

Acceptance:

- A short `engine/WgpuRenderer/docs/grass-system-plan.md` contract references
  source reality.
- No ABI ambiguity remains.

### GRASS-001 - Procedural blade integration

- Add `GrassSystem`, the command/batch ABI, timers, and feature gate.
- Render one fixed procedural test patch.
- Join the existing prepass and colour pass.
- Confirm water sees grass in the frozen scene.
- Keep planar reflections and grass shadows disabled.

Acceptance:

- Grass-disabled output is unchanged.
- Correct terrain/object depth.
- Clean create, resize, map reload, device loss path, and shutdown.

### GRASS-002 - Stateless GPU placement

- Add deterministic ring enumeration and compute compaction.
- Add three visible buffers and three indirect args.
- Attach to the shared exact heightmap.
- Add distance, frustum, bounds, water, and exact slope filters.
- Add counters and overflow diagnostics.

Acceptance:

- Rotation does not move grass.
- Walking away and back reproduces identical blades.
- Teleporting requires no special recovery and returns identical placement.
- No per-blade CPU work or per-tile draw calls.

### GRASS-003 - Existing geography and basic ecology

- Upload `GeographyInfo`.
- Exclude water, roads/tracks, off-map cells, and hard/full cells.
- Add forest/gradient density response.
- Add the first surface/ecotype map with a conservative fallback.
- Add debug views for geography, ecology, slope, and rejection reason.

Acceptance:

- No broad underwater or off-map grass.
- Roads and obstacle-heavy cells are suppressed.
- Existing islands work without a hand-painted mask.

### GRASS-004 - Reference-quality appearance and wind

- Port the reference blade curve, colour gradient, root AO, clumping,
  transmission philosophy, and projected-width concept.
- Use live `Landscape::GetWind()`.
- Add shared fog, terrain shadow, CSM receive, sky visibility, and local lights.
- Add near/mid/far geometry and stable transitions.

Acceptance scenes:

- Noon, sunset, overcast, night, fog.
- Meadow, forest edge, coast, slope.
- Standing, prone, zoomed FOV, rapid rotation, fast driving.

### GRASS-005 - Near shadows

- Join the existing cascade target.
- Add per-cascade culling, short shadow distance, reduced density, and shared
  deformation.

Acceptance:

- No static/detached shadows.
- No cascade-sized timing spikes.
- No distant grass flooding shadow maps.

### GRASS-006 - Interaction

- Add analytic character and vehicle interactors.
- Add explosion flattening.
- Add persistent bend field only after analytic interaction is accepted.

Acceptance:

- Walking and prone movement produce appropriate shapes.
- Vehicles leave directional, recovering tracks.
- No CPU readback.

### GRASS-007 - Fine exclusions, species, and optimisation

- Add exact road/building/runway override masks and optional author maps.
- Add meadow, dry, forest, reed, and rocky presets.
- Profile candidate compaction, vertex cost, overdraw, prepass, and shadows.
- Add cluster culling, Hi-Z grass re-culling, or persistent clipmap caching only
  when a measured bottleneck justifies it.

Acceptance:

- Visual and timing report across quality presets.
- Every optional optimisation has an on/off A/B measurement.

## Diagnostics and quality presets

Expose:

- Enumerated, accepted, and rejected candidate counts.
- Visible instances per LOD.
- Shadow candidates per cascade.
- Buffer capacity and overflow.
- Placement, prepass, colour, shadow, and interaction GPU timings.
- Freeze time/wind/interaction.
- LOD colours.
- Rejection reason.
- Geography/ecology/slope/interaction overlays.

Initial High targets at 1080p:

| Metric | Engineering target |
| --- | ---: |
| Grass CPU cost | <= 0.15 ms |
| Placement/compaction | <= 0.35 ms |
| Prepass + colour | <= 1.50 ms |
| Grass shadows | <= 0.70 ms |
| Total | approximately <= 2.5 ms |
| Main colour submissions | 1-3 indirect draws |
| Per-blade CPU work | none |

These are profiling goals, not promises. Start lower on density and distance,
then increase only with measured headroom.

## Mandatory validation

For every C++/Rust/WGSL implementation change:

```powershell
cmake --build build/win-x64-clang-rwdi --target PoseidonGame
```

Deploy the matching EXE and DLL together, then launch with:

```powershell
cmd /c ".\ColdWarAssault.exe --render wgpu --window --dev --log-file cwr.log"
```

Confirm:

```text
[INFO] [GRAPHICS] Wgpu: creating renderer WGPU (Rust / wgpu)
```

Also run Rust formatting/tests appropriate to the modified module and validate
all WGSL entry points through naga tests before in-game testing.

Test at minimum:

- Flat, rolling, steep, coastal, forest, road, town, runway, and map-edge land.
- Standing, prone, third person, zoom, fast rotation, driving, and teleport.
- Midday, sunset, night, fog, cloud shadow, rain/wet look, and each CSM cascade.
- Grass disabled, terrain/map reload, zero visible candidates, buffer overflow,
  missing optional ecology/interaction texture, shadows disabled, water
  disabled, resize, and device recreation.

## Non-negotiable rules

1. No engine object per patch or blade.
2. No per-tile draw submissions.
3. No duplicate terrain heightmap.
4. No bilinear approximation for root height.
5. No frame-dependent placement randomness.
6. No camera-relative sliding or reseeding.
7. No alpha-blending the full field.
8. No duplicated deformation logic between depth, colour, and shadow.
9. No grass in every shadow cascade at full density.
10. No off-map grass on the renderer's extended seabed border.
11. No tree-canopy normal model applied unchanged to grass.
12. No interaction dependency in the basic renderer.
13. No OpenGL changes during the initial WGPU implementation.
14. No clipmap/cluster complexity without profiling evidence.
15. No copied texture/audio/model asset without separate licence verification.

GodotGrass is MIT-licensed. Preserve an MIT notice for copied or substantially
adapted code. Prefer reimplementing the small algorithms from the described
techniques and generating the blade procedurally, so no reference mesh or noise
asset needs to ship.

## First implementation task

Start with GRASS-000, then GRASS-001.

The first visible milestone is deliberately modest: one deterministic,
procedural, terrain-attached patch integrated into the existing prepass and
colour path, correctly visible through/behind water. Once that contract is
proven, stateless GPU placement can scale the same blade to the battlefield
without committing early to a complicated streaming cache.
