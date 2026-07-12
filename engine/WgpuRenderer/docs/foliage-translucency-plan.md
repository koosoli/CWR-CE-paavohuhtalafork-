# Foliage translucency plan (emulated subsurface scattering for low-poly vegetation)

**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust). **Status:** Stage 1 IMPLEMENTED (uncommitted,
2026-07-12), cutout-gated, modest defaults ON; Stages 2–3 still planned.

## 0. Implementation status

- **Stage 1 (leaf SSS, cutout-gated) — DONE + REWORKED, needs visual tuning.** Live in `shade()` for
  every alpha-tested cutout (`is_foliage = alpha_ref > 0` on both draw paths). Knobs travel through
  `WgrRenderParams.foliage` → the per-camera Frame UBO (`frame.foliage` / `frame.foliageb`, appended after
  `inv_view_proj`; `CameraGroup::new` bind_size +32) and are tunable live on the **Foliage** ImGui tab
  (`FoliageSettings` in Engine.hpp → `EngineWgpu::_foliage` → `PushRenderParams`). Rust crate + naga
  compose tests green; C++ not yet built here. Naming: `foliageb` not `foliage2` (naga_oil digit rule).
  - **Rework (2026-07-12) after first visual pass.** The first cut over-brightened and looked flat/glowy
    on distant high-LOD billboards — three additive fill terms (wrapped-front + back + a *normal-independent*
    forward-scatter) stacked onto the full HDR sun. Replaced with a cleaner model (see §3): **base Lambert
    unchanged** (sunlit foliage now matches terrain, no over-bright), a **single normal-bent DICE
    transmission** (couples to orientation → no flat view-only sheet), a small **wrap fill**, and a
    **camera-distance fade** on wrap+transmission so distant billboards revert to plain Lambert. Knobs
    changed accordingly: `foliage = (trans_scale, distortion, trans_power, wrap)`, `foliageb =
    (ambient_boost, normal_bend, crown_y_offset, fill_fade_end)`. Defaults: trans 0.3, distortion 0.3,
    power 4, wrap 0.25, ambient 1.0, fade_end 150 m.
  - **Note:** the dawn "bright foliage over black terrain" split is mostly the terrain sun-shadow (foliage
    lit, valley floor shadowed) + still-low ambient — not a foliage bug. The rework targets the *look*
    (flatness/glow), not that contrast; real relief there wants more ambient / GI, not dimmer foliage.
  - **Rev 2 (2026-07-12): distance-faded ambient boost + cheap GI.** The ambient boost now fades with
    distance (shares `fill_fade_end`) so it's a near-field evening-out, and a new **cheap-GI** term scales
    foliage sky-ambient by the terrain's local light level (`ambient *= mix(1, 1 - terrain_s, gi_strength)`)
    — sunlit areas keep full/boosted ambient, foliage in a mountain's shadow settles toward the shadowed
    terrain instead of glowing in the dark. New knob `gi_strength` (default 0.7) in a third vec4
    `foliagec`; WgrFoliage 32→48 B, WgrRenderParams 288→304, Frame-UBO append +48. Could later generalise
    the GI scaling to terrain/objects (a scene-wide look change), but it's foliage-scoped for now.
  - **Known-open: bushes weirdly dark in direct sun.** A bush card whose normal faces away from the sun
    gets `front = max(N·L, 0) = 0`, so it stays dark in full sun (only ambient + faded fill). This is the
    back-facing-card problem and the direct motivation for **Stage 3 spherical normals** (a radial canopy
    normal gives the visible side a sensible `N·L`) — bushes are its sweet spot. Two-sided lighting is the
    cheaper stopgap if Stage 3 slips.
- **Stage 3 (spherical/canopy normals for bushes AND trees) — DONE (GPU-driven path), user-verified for
  bushes.** `vs_gpu` bends a canopy vertex's normal toward `normalize(world_pos − (inst.center − cam_pos +
  crownY·ŷ))` (radial crown normal), blended by a per-kind bend. **Gated by TWO things:** a per-instance
  flag (`WGR_INSTANCE_CANOPY_BUSH` = bit 0 / `_TREE` = bit 1 of the already-plumbed `WgrInstance.flags`,
  set in `BuildGpuInstance` from `GetMapType()`) **AND** the section being cutout
  (`section_materials[rec.section].alpha_ref > 0` — group-1 is `VERTEX_FRAGMENT`-visible). The cutout gate
  is what lets trees participate: only leaf/canopy sections bend, the **solid trunk keeps its real normal**.
  Bush and tree pick different knobs — bush: `foliageb.y`/`.z` (bend 0.85, crownY 0.27); tree: `foliagec.y`/`.z`
  (bend 0.7, crownY 2.5, a bigger lift because the bounding-sphere centre sits mid-trunk). **No new Rust
  structs** — `flags` was free + piped end-to-end (cull ignores it); the two tree knobs took spare
  `foliagec` lanes (no size change). Applied at all distances (a normal is smoothing, not the glowy fill).
  Fragment shader unchanged. Fixes the back-facing-card problem (a card facing away from the sun now shades
  from the radial normal instead of going black).
- **Stage 2 (MapType gate) — PARTIAL.** The bush/tree flags above are the first `MapType`-derived signals.
  The SSS *fill* (§3.1–3.5) is still gated only on `is_cutout` (so fences etc. still get it); narrowing that
  to vegetation is the remaining Stage-2 work (per-draw `WgrDraw3D::flags` bit + GPU-driven section flag).
- **Forests / distant LODs — TODO.** `ForestPlain` is a multi-tree cluster whose single `center` is
  meaningless per-tree, so radial normals off it would be wrong — excluded. User's idea for distant forests:
  **flood-fill-split the forest mesh into per-tree submeshes** so each gets its own crown centre. Plus some
  of the 1 km "looks bad" is non-lighting (billboard aliasing, LOD-cross-fade popping) — separate threads.

## 1. Motivation

OFP/Resistance vegetation is extremely low-poly. Base-game trees are two alpha-tested cards (impostors
in modern terms); Resistance trees add a real trunk plus separate leaf/canopy sections, but the canopy
is still a handful of back-to-back one-sided alpha-tested polys. Bushes, reeds and grass clumps are the
same construction.

Under the current physically-motivated look this reads badly at harsh sun angles (sunrise/sunset, high
noon). A card has only 1–2 distinct normals, so the sun term `max(dot(N, L), 0)` is effectively binary:
the card facing the sun blows out, the card facing away goes to (near) black. Directional sky-irradiance
ambient (`sky_irradiance(n)`, SH-9) softens this a little but is itself directional, so the shadow-side
card — normal pointing away from both sun *and* the bright part of the sky — stays dark. The bright/dark
split across a single bush is dramatic and unnatural, and it gets worse at higher LODs (fewer polys, more
extreme normals).

Real foliage doesn't do this because thin leaves are **translucent**: light transmitted *through* the
leaf keeps the shadow side glowing, and forward-scattered light produces the bright backlit halo when the
sun is low behind the canopy. Both are absent from a pure reflectance model. We can emulate them cheaply
in the fragment shader, **without modifying any assets**.

## 2. The signal — which draws are "foliage canopy"

Two independent classifications exist in the engine; the precise canopy signal is their **intersection**.

- **Per-object, semantic — `MapType`.** `LODShape::GetMapType()`
  ([`Shape.hpp:931`](../../Poseidon/Graphics/Rendering/Shape/Shape.hpp)) returns a byte parsed at model
  load from the P3D `map=` property ([`ShapeLOD.cpp:1009`](../../Poseidon/Graphics/Rendering/Shape/ShapeLOD.cpp),
  enum in [`MapTypes.hpp:7`](../../Poseidon/World/MapTypes.hpp)). The vegetation members are `MapTree`,
  `MapSmallTree`, `MapBush`, `MapForestBorder`, `MapForestTriangle`, `MapForestSquare`. This says *"this
  whole model is a plant."* It is already serialized on the shape but is **not currently plumbed into the
  wgpu path** (no `GetMapType` reference exists under `engine/WgpuRenderer/`).
- **Per-section, material — alpha-test cutout.** Leaves/needles are `IsTransparent` chromakey sections
  ([`Types.hpp:325`](../../Poseidon/Core/Types.hpp)); there is also a `GrassTexture` bit. The wgpu backend
  already consumes this: `BlendForSpec`/`alpha_ref` ([`EngineWgpu.cpp:680`](../../../WgpuRenderer/EngineWgpu.cpp))
  become the shader's `alpha_ref > 0`, which `shade()` already calls `is_cutout` and treats as foliage
  (it drives the existing `foliage_shadow_ao` canopy darkening,
  [`shading.wgsl:109`](../rust/src/shaders/shading.wgsl)).

**Canopy = `MapType ∈ vegetation` AND section is alpha-test cutout.** The `MapType` gate excludes the
trunk sections of Resistance trees (solid, non-cutout — they should light normally) and, crucially,
excludes non-plant cutouts (chain-link fences, ladders, radio antennas, window frames) that would
otherwise pick up a green translucent glow at sunset if we keyed on `is_cutout` alone.

> **Granularity note.** `MapType` is per-`LODShape` (whole object); `alpha_ref`/cutout is per-section.
> So we only need to plumb **one new bit** — "this shape is vegetation" — and combine it in-shader with
> the `alpha_ref > 0` the section already carries. `is_foliage = is_vegetation && is_cutout`.

## 3. The shading model (as implemented, post-rework)

All of this lives in the shared `shade()` in [`shading.wgsl`](../rust/src/shaders/shading.wgsl), in the
`sky_lit` branch, reused verbatim by both draw paths (per-draw `fs_main` in `shader3d.wgsl` and GPU-driven
`fs_gpu` in `gpu_driven.wgsl`) since both call `shade()`. Every input is already present: surface normal
`N = nrm`, sun dir `frame.sun_dir_world`, sun colour `frame.sun_diffuse.rgb`, sun visibility `sun_vis`
(CSM ⊕ terrain-shadow), albedo, and camera-relative `world_pos` (so `V = normalize(-world_pos)`). Let
`L = -frame.sun_dir_world.xyz` (surface→light).

The design principle after the first visual pass: **do not brighten the sunlit side** (that's what made
foliage read as glowing versus terrain), and **do not paint a flat view-only glow** (that's what made
distant high-LOD billboards look wrong). So the sunlit response is left as plain Lambert — identical to
terrain — and every *fill* term (a) lifts only the dark/backlit side, (b) couples to the surface normal,
and (c) fades out with distance.

**(1) Base Lambert — unchanged, matches terrain.**

```wgsl
let front = max(dot(N, L), 0.0);   // a leaf's sunlit side lights exactly like the ground it sits on
```

**(2) Transmission — the single normal-bent DICE fast-SSS term** (Barré-Brisebois / Bouchard, GDC 2011),
replacing the old separate view-independent "back" + normal-independent "forward" terms. Bending the
light direction by the normal (`distortion`) is what ties it to orientation, so it lifts the backlit /
shadow side without becoming a uniform sheet across a billboard:

```wgsl
let lt = normalize(L + N * distortion);
let trans = pow(clamp(dot(V, -lt), 0.0, 1.0), max(trans_power, 1.0)) * trans_scale;
```

Strong when the view looks toward the (bent) transmitted light and the front is dark; ~0 on the sunlit
side, so it never doubles the lit side. Tinted green/leaf-coloured via the shared `rgb = albedo * lit`.

**(3) Terminator-wrap fill — dark-side only.** The extra lift the wrapped half-Lambert adds *over* base
Lambert (0 on the lit side, where the wrapped value equals `front`):

```wgsl
let wrap_fill = max((dot(N, L) + wrap) / (1.0 + wrap), 0.0) - front;
```

**(4) Distance fade — the SSS fill is near-field.** Wrap + transmission fade out with camera distance so
distant / low-LOD billboards revert to plain Lambert (shading like terrain) instead of glowing as a flat
sheet. `fill_fade_end <= 0` disables it.

```wgsl
var fill = wrap_fill + trans;
if (fill_fade_end > 0.0) { fill *= 1.0 - smoothstep(fill_fade_end * 0.5, fill_fade_end, length(world_pos)); }
```

**(5) Ambient boost.** A plain multiplier on the SH ambient for foliage (1 = off).

**Composed foliage sun term** (replaces `sun = m_emissive + ambient + frame.sun_diffuse.rgb * ndotl * sun_vis`
for `is_foliage`):

```wgsl
sun = m_emissive + ambient * ambient_boost
    + frame.sun_diffuse.rgb * (front + fill) * sun_vis;
```

All fill is gated by `sun_vis`, so a leaf in cast shadow neither transmits nor glows.

**Interaction with `foliage_shadow_ao`.** That existing term *darkens* terrain-shadowed cutout foliage
(dense canopy self-occlusion the world-space mask can't model). It composes cleanly: the fill is gated on
`sun_vis`, so a fully terrain-shadowed leaf gets no fill and still darkens via `foliage_shadow_ao`. The
two act on opposite lighting states (lit-but-facing-away vs. in-cast-shadow) and should be tuned together.

## 4. Plumbing

`shade()` already threads `is_cutout: bool` and `is_translucent: bool` — add `is_foliage: bool` the same
way (minimal surface). Feed it `is_vegetation && is_cutout`, where `is_cutout` is the existing `alpha_ref > 0`
and `is_vegetation` is the new bit.

- **Per-draw path** ([`shader3d.wgsl`](../rust/src/gfx3d/shader3d.wgsl) `fs_main`): set a spare bit in
  `WgrDraw3D::flags` — e.g. `DRAW3D_VEGETATION` — when the object's shape is vegetation
  (`GetMapType()` ∈ veg set), decided C++-side where draws are emitted in `EngineWgpu.cpp`. The `flags`
  word only uses `ON_SURFACE` (bit 0) + z-bias (bits 8–9) today ([`ffi.rs:400`](../rust/src/ffi.rs)),
  so a fresh bit is free. Unpack in `fs_main`, `&&` with `alpha_ref > 0.0`, pass to `shade()`.
- **GPU-driven path** ([`gpu_driven.wgsl`](../rust/src/gfx3d/gpu_driven.wgsl) `fs_gpu`, default-on): mark
  it once at model registration. `SectionMaterialGpu` has a genuine spare `_pad: u32`
  ([`cull.rs:117`](../rust/src/gfx3d/cull.rs)) — set it from the shape's `MapType` when sections are
  registered (matching `WgrModelMaterial` on the C++ side; that header struct would need the field
  exposed). Read per-section in `fs_gpu`, `&&` with the section's `alpha_ref`, pass to `shade()`.

Because the vegetation bit is per-shape and cutout is per-section, a Resistance tree naturally gets the
effect on its canopy sections and normal lighting on its trunk, with no per-section MapType needed.

**Derivative-before-discard rule:** any branch added must stay after the `dpdx/dpdy(world_pos)`
computation and before `discard` — `shade()` is already called past that point, so putting the whole
model inside `shade()` respects it automatically.

## 5. Tuning knobs (ImGui + `WgrRenderParams`)

Per the render-params consolidation convention ([`render-params-consolidation-plan.md`](render-params-consolidation-plan.md)),
all of these ride in `WgrRenderParams` behind one `wgr_set_render_params`, exposed on a new **Foliage**
ImGui tab — no per-setting FFI:

- `trans_scale`, `distortion`, `trans_power` — DICE transmission strength / normal-bend / lobe tightness (§3.2).
- `wrap` — terminator-wrap fill, 0 = off (§3.3).
- `fill_fade_end` — distance (m) where wrap+transmission fade out, 0 = never (§3.4).
- `ambient_boost` — SH ambient multiplier, 1 = off (§3.5).
- `normal_bend`, `crown_y_offset` — Stage-3 spherical normals (§7); inert until a crown centre is plumbed.

Defaults (post-rework): trans 0.3, distortion 0.3, power 4, wrap 0.25, fade_end 150 m, ambient 1.0; dialled
in live against dawn/noon/dusk presets.

## 6. Staging

- **Stage 1 — look first, cutout-gated.** Implement §3 in `shade()` gated on the **existing `is_cutout`**
  only (no `MapType` plumbing yet). Zero new FFI/flags; immediate on-screen result to tune with the ImGui
  knobs. Accepts temporary over-application to non-plant cutouts (fences etc.).
- **Stage 2 — precision gate + per-type selector.** Plumb `MapType` through both paths (§4) and switch
  the shader gate to `is_vegetation && is_cutout`, removing the effect from fences/ladders/etc. The same
  byte doubles as a per-type selector for the constants in Stage 3. Wanted both to stop over-application
  *and* to enable per-type foliage look — not merely a fallback.
- **Stage 3 — spherical (canopy) normals (§7).** The bush win. Not "optional/if-needed": bushes are the
  most common foliage object and the cleanest case for this, so it's a primary deliverable, done alongside
  §3. Prototype on the GPU-driven path (crown `center` already present), high bend for `MapBush`, low/none
  for trees and grass per Stage 2's selector.

## 7. Runtime spherical (canopy) normals — the big win for bushes

A complementary, orthogonal lever, and for **bushes** (`MapBush`) potentially the *larger* visible
improvement of the whole plan. Bushes are the most common foliage object across the stock maps, and they
are the ideal case for this technique (see below). "Spherical normals" shade the crown as a smooth
sphere/ellipsoid instead of flat cards by pointing the leaf normal outward from the crown centre. The
classic AAA form (SpeedTree) **bakes** proxy-hull normals into each model's vertex data — per-asset
authoring we can't do. But the same idea can be approximated **at runtime**, no asset edits:

```wgsl
let sph_n = normalize(world_pos - center_cr);          // center_cr = crown centre, camera-relative
let n_veg = normalize(mix(nrm, sph_n, foliage_normal_bend));  // bend, not replace
```

We already carry the centre on the GPU-driven path: `InstanceGpu.center.xyz` is the transformed
bounding-sphere centre ([`cull.rs:42`](../rust/src/gfx3d/cull.rs)); subtract `cam_pos` for the
camera-relative crown centre. The per-draw path only carries `BoundingCenter().y` today
(`conform0.w`) and would need the full centre plumbed (one more vec, feasible — conform is already
per-draw).

**What it fixes vs. what it doesn't.** Spherical normals smooth the *reflectance* gradient — they turn
the sunlit side from banded flat cards into a rounded mass. They do **not** light the hemisphere facing
away from the sun (that's what §3's transmission/forward-scatter is for). So this stacks under the SSS
work, it doesn't replace it. **Blend, don't replace** (`mix`, hence `foliage_normal_bend ∈ [0,1]`):
full replacement discards the front/back card facing that §3(a)'s `dot(-N, L)` relies on.

**Why bushes are the sweet spot.** A bush is a single foliage blob with **no trunk**, so its
bounding-sphere centre *is* the crown centre — `normalize(pos - center)` needs no offset hack and gives
clean outward radial normals directly. The stock bush cards' own normals are largely arbitrary, so we can
lean **hard** toward the spherical normal (high `foliage_normal_bend`, near-full replacement) with little
to lose. Bushes being the dominant foliage object, this is where the technique pays off most.

**Per-`MapType` behaviour** (why this makes Stage 2's `MapType` byte do double duty — it's not just an
on/off gate, it selects per-type constants):
- **`MapBush`** — high bend (the sweet spot above), plus modest translucency.
- **`MapTree` / `MapSmallTree`** — low/no bend: the bounding centre sits mid-trunk so the radial normal is
  approximate. Either bias the centre up into the crown, or skip the bend and rely on §3 translucency.
- **reeds / vertical grass** — no radial normal (a blade wants an up-ish normal); translucency only.

Because the payoff is concentrated on the dominant, cleanest case, spherical normals are worth doing
alongside the §3 work rather than deferring — but they *do* depend on a sensible crown centre + shape,
whereas §3's translucency does not, which is why §3 is still the robust baseline.

Prototype on the GPU-driven path first (`center` already present). Add `foliage_normal_bend` (and, for the
tree case, an optional crown-centre Y offset) to the same `WgrRenderParams` Foliage tab, ideally as
per-`MapType` values once Stage 2's gate is in.

## 8. Why this is cheap

No new passes, no new bind groups, no new textures. A few ALU ops in the object fragment shader, one
new bool parameter to `shade()`, one spare flag bit (per-draw) / spare `_pad` field (GPU-driven), and a
handful of scalar knobs in the already-uploaded `WgrRenderParams`. Constant thickness (no thickness map)
is acceptable for geometry this simple.

## 9. Distant forests — per-tree crown centres (PLANNING, future session)

`ForestPlain`/`Forest` is one authored, merged `LODShapeWithShadow` (all tree cards baked into one
vertex/face table); it retains **no** per-tree placement data, and `Animate` only terrain-conforms the
existing vertices (`ObjectClasses.cpp`). So the single per-instance `inst.center` is meaningless per-tree,
and Stage-3 spherical normals are deliberately **excluded** for forests today
([`EngineWgpu.cpp` BuildGpuInstance](../EngineWgpu.cpp)). To include them, each tree needs its own crown
centre, and the only source is **flood-fill connected-components** on the merged mesh (position-weld with
an epsilon, since `Optimize`/`SortVertices` may merge coincident verts; risk = touching canopies at a
boundary merging into one component).

**Two approaches — pick by goal:**

- **(A) Per-vertex crown-centre attribute — lighting only. RECOMMENDED for the shading goal.** Flood-fill
  each LOD mesh into components, compute each component's centroid, and bake the owning component's centre
  as a **per-vertex attribute** (or a per-vertex `u16` index into a small per-model centre table — cheaper;
  could ride spare bits of the existing `conform` vertex `u32`). The mesh is **not physically split** — one
  draw per LOD, unchanged. `vs_gpu` reads the per-vertex centre instead of `inst.center`; the rest of the
  Stage-3 path (bend, gate, tree/bush knobs) is reused verbatim, and the ForestPlain exclusion is lifted.
  This is small and stays renderer-side (flood-fill runs once in `RegisterGpuModel`, where the base mesh +
  sections are already in hand).

- **(B) Physical per-tree submeshes / instances — the big engine feature.** Actually split the forest into
  separate objects/instances. Enables per-tree GPU culling and per-tree LOD, but is a genuine engine-side
  change, not renderer-contained.

**The two hard parts (noted by the author) are Approach-B problems that Approach A avoids:**

1. **Cross-LOD + occlusion grouping** ("tree #3 in LOD0 = tree #3 in LOD2 = tree #3 in the occluder").
   *Not needed for A:* each LOD flood-fills independently and a vertex's centre comes from its own LOD's
   geometry; GPU-driven LOD is selected per-*instance* (the whole forest is one LOD per frame), and a
   tree's centroid sits at ~the same xz in every LOD, so LOD switches don't pop the normals. No component
   correspondence across LODs. The occlusion/shadow mesh needs no centres at all (depth-only), so it drops
   out. *Required for B*, where per-tree LOD/cull needs the matched chain + occluder per tree.

2. **Trees staying in place / conform + skew.** *Near-moot for A:* nothing is moved or split, so trees
   can't drift. The one wrinkle — the baked centre is pre-conform while vertices are GPU-conformed (mode-1
   plane) at runtime — is solved by evaluating the **same** per-instance conform plane (`inst.conform*`) at
   the crown's xz in the shader. Skewed t1/t2 forests register **rigid** (no conform), so they need nothing.
   *For B*, keeping split submeshes co-located under conform/skew is a real ownership problem.

**Open items for the future session:** the per-vertex centre encoding (full `vec3` attribute vs `u16`
index + per-model table vs packing into the `conform` word); the flood-fill weld epsilon + the
touching-canopy merge risk; confirming forest LOD is per-instance (so no cross-LOD matching); and whether
to also conform the crown-centre Y in-shader or accept the small static offset. Non-lighting distant-forest
issues (billboard aliasing, LOD cross-fade popping, the impostor art) are **separate** from this and not
addressed by crown normals.
