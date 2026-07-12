# Foliage translucency plan (emulated subsurface scattering for low-poly vegetation)

**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust). **Status:** PLAN (2026-07-12). Not implemented.

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

## 3. The shading model

All of this lives in the shared `shade()` in [`shading.wgsl`](../rust/src/shaders/shading.wgsl), in the
`sky_lit` branch around lines 78–89, and is reused verbatim by both draw paths (per-draw `fs_main` in
`shader3d.wgsl` and GPU-driven `fs_gpu` in `gpu_driven.wgsl`) since both call `shade()`. Every input is
already present: surface normal `nrm`, sun dir `frame.sun_dir_world`, sun colour `frame.sun_diffuse.rgb`,
sun visibility `sun_vis` (CSM ⊕ terrain-shadow), albedo, and camera-relative `world_pos` (so
`V = normalize(-world_pos)`).

Let `L = -frame.sun_dir_world.xyz` (surface→light), `N = nrm`, `V = normalize(-world_pos)`.

**(a) Back-transmission — view-independent, evens out the two sides.** The dominant fix. Light through
the leaf lands on the shadow-side card:

```wgsl
let back = max(dot(-N, L), 0.0);            // shadow-side card faces away from sun
transmission = frame.sun_diffuse.rgb * back * sun_vis * trans_scale;
```

Tint by albedo (`* albedo` happens via the shared `rgb = albedo * lit`, so transmission naturally reads
green/leaf-coloured). Scaled by `sun_vis` so a leaf in cast shadow doesn't transmit.

**(b) Forward scatter — view-dependent, the backlit halo.** The glow when looking toward a low sun
through the canopy — directly the sunrise/sunset case:

```wgsl
let fwd = pow(clamp(dot(V, -L), 0.0, 1.0), trans_power);   // looking into the sun
forward = frame.sun_diffuse.rgb * fwd * sun_vis * trans_fwd_scale;
```

(a) + (b) together are the standard DICE "fast subsurface scattering" foliage approximation
(Barré-Brisebois / Bouchard, GDC 2011), minus the thickness map we don't have — a constant thickness is
fine for these cards.

**(c) Optional terminator wrap — softens the front terminator.** Replace the front `ndotl` for foliage
with a half-Lambert-style wrap:

```wgsl
let ndotl_veg = max((dot(N, L) + wrap) / (1.0 + wrap), 0.0);   // wrap ~ 0.5
```

**(d) Fallback ambient boost.** If (a)–(c) still leave the shadow side too dark, a plain multiplier on
the SH ambient for foliage is trivial:

```wgsl
ambient *= foliage_ambient_boost;   // 1.0 = off
```

**Composed foliage sun term** (replaces `sun = m_emissive + ambient + frame.sun_diffuse.rgb * ndotl * sun_vis`
for `is_foliage`):

```wgsl
sun = m_emissive
    + ambient * foliage_ambient_boost
    + frame.sun_diffuse.rgb * ndotl_veg * sun_vis     // (c) front reflectance, wrapped
    + transmission                                     // (a) back transmission
    + forward;                                          // (b) forward-scatter halo
```

**Interaction with `foliage_shadow_ao`.** That existing term *darkens* terrain-shadowed cutout foliage
(dense canopy self-occlusion the world-space mask can't model). It composes cleanly with this plan: (a)/(b)
are gated on `sun_vis`, so a fully terrain-shadowed leaf gets no transmission/forward glow and still
darkens via `foliage_shadow_ao`. The two act on opposite lighting states (lit-but-facing-away vs.
in-cast-shadow) and should be tuned together.

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

- `foliage_trans_scale` — back-transmission strength (a).
- `foliage_fwd_scale`, `foliage_fwd_power` — forward-scatter strength + tightness (b).
- `foliage_wrap` — front terminator wrap, 0 = off (c).
- `foliage_ambient_boost` — SH ambient multiplier, 1 = off (d).

Defaults start conservative (transmission on, forward-scatter modest, wrap ~0.3, ambient boost 1.0) and
get dialled in live against dawn/noon/dusk presets.

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
