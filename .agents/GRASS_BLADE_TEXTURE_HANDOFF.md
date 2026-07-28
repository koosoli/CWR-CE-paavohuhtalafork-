# Handoff — adding single-blade photo textures to near-LOD grass

Written for whoever picks this up next. Everything below is verified against the
working tree on branch `new-water-and-grass-system`, not remembered.

---

## 1. Where the textures go

**Source of truth (committed to the repo):**

```
assets/grass/<name>.png
```

**Runtime location — this is the part that bites.** Textures are read with a
path relative to the **working directory**, which is the *game install*, not the
repo:

```
D:\SteamLibrary\steamapps\common\ARMA Cold War Assault\assets\grass\<name>.png
```

So deploying is now a **three**-part copy, not the two in
[README.md](README.md): `PoseidonGame.exe` → `ColdWarAssault.exe`,
`wgpu_renderer.dll`, **and** the `assets/` tree. Miss the third and the texture
silently fails to load and grass quietly drops to its procedural fallback.

`WGR_GRASS_TUFT` overrides the clump path for quick A/B; add an equivalent env
var for blades if useful.

---

## 2. What already exists

The near blade is **not** a billboard. It is 60 vertices of real geometry: two
crossed ribbons, five segments each, that bend per-blade in the wind. The
silhouette is geometric. A texture supplies *surface detail only*.

| thing | where |
|---|---|
| procedural atlas generator | `engine/WgpuRenderer/rust/src/grass/blade_atlas.rs` |
| GPU-side wiring, bind groups | `engine/WgpuRenderer/rust/src/grass/mod.rs` |
| shaders | `grass/grass.wgsl`, `grass/grass_shadow.wgsl` |
| C++ texture loading | `engine/WgpuRenderer/TerrainWgpu.cpp` → `UploadGrassTuft()` |
| FFI | `rust/src/ffi.rs` + `include/wgpu_renderer.hpp` |

The blade atlas today is a `texture_2d_array`, **8 layers**, `64 x 256` each,
`Rgba8UnormSrgb`, fully opaque. Bound at **group 2, binding 3** (texture) and
**binding 4** (sampler). The clump texture for the mid ring sits at **binding 5**
and is loaded from a real PNG already — `UploadGrassTuft()` is your worked
example for the whole path.

### The UVs are already there

`vs_grass` writes `out.blade_uv`:

- `.x` — 0 or 1, which side of the ribbon (from the `left` corner bit)
- `.y` — `1.0 - height_t`, so **0 is the blade tip, 1 is the root**
- `.z` — species index, used as the array layer
- `.w` — distance fade, from `blade_texture_strength()` (full detail to 14 m,
  gone by 35 m)

Sampled in `fs_grass`. No new plumbing needed to texture a blade — only a
different image in the array.

---

## 3. What a single-blade texture must look like

One blade, **not** a clump. The clump image (`meadow-grass-clump-alpha-1024.png`)
is for the mid ring's crossed cards and will not map onto blade geometry.

- **One blade, near-vertical, filling the frame top to bottom.** Root at the
  bottom edge, tip at the top.
- **Blade fills the frame horizontally too.** The geometry provides the taper;
  the image should be the blade's *surface*, not a blade floating in space.
- **Tall aspect** — 64×256 or 128×512. All layers must share one size (array
  requirement).
- **Flat, even, diffuse lighting.** No sun, no cast shadow, no baked AO. The
  engine lights it; anything baked in fights that. This is half of why the 2001
  game texture looked wrong.
- **Green, R > B.** See the trap in §6.
- **Opaque preferred**, alpha ignored — see §5.

Prompt spec that produced a good clump, adapted for blades:

```text
A single blade of wild meadow grass, photographed straight on, filling the
frame from bottom to top. The blade runs near-vertical and spans most of the
frame's width; this is a close study of one blade's surface, not a blade seen
at a distance. Visible central fold/midrib, fine lengthwise veins, subtle
irregular chlorophyll mottling, slight browning near the tip.
Flat even overcast lighting. No directional sun, no cast shadows, no
highlights, no ambient occlusion, no vignette. Sharp focus throughout, no
depth of field. Natural mid-green, not grey, blue-green or teal.
Photographic realism, no stylisation. 128 x 512 PNG.
```

---

## 4. How to implement

Mirror the clump path exactly — it works and is tested.

### 4a. Rust: build the array from supplied images

In `blade_atlas.rs`, add alongside `create()`:

```rust
pub fn create_from_images(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    layers: u32,      // must equal LAYERS (8) — see §6
    rgba: &[u8],      // layers * width * height * 4, layer-major
) -> Option<wgpu::TextureView>
```

Copy the mip loop out of `create()`. Use the **plain `downsample()`** for opaque
blades. Use `downsample_cutout()` + `preserve_coverage()` **only** if the images
carry cutout alpha — see §5.

### 4b. Rust: accept and rebind

In `mod.rs`, add `set_blade_atlas(...)` modelled on `set_tuft()`. The critical
part is at the end of `set_tuft`: **a bind group captures the texture view it was
built with**, so a new texture means rebuilding **all three** data bind groups
(`data_bind`, `mid_data_bind`, `far_data_bind`). Forget one and that LOD keeps
sampling the old texture with no error.

Keep a `have_blade_atlas` flag and fall back to the procedural atlas when false.

### 4c. FFI

`wgr_grass_set_blade_atlas(renderer, width, height, layers, rgba)` in `ffi.rs`,
declared in `wgpu_renderer.hpp`. Copy `wgr_grass_set_tuft` including the null and
zero-size guards and `catch_unwind`.

### 4d. C++ loading

In `TerrainWgpu::UploadGrassTuft()` (or a sibling), load N PNGs with `stb_image`
(already vendored, already included), assert identical dimensions, concatenate
layer-major into one buffer, call the FFI once. Log at **INFO** when absent — a
missing optional asset is not a fault.

---

## 5. Opaque vs alpha cutout — decide deliberately

The near ring is the densest, highest-overdraw geometry in the frame.

- **Opaque (current, recommended).** No `discard`, early-Z intact. The ribbon
  geometry already gives the silhouette, so a photo's alpha buys nothing here.
- **Cutout.** If you alpha-test, you must `discard` in **both** `fs_grass` *and*
  `fs_grass_prepass`. Miss the prepass and it stamps opaque rectangles into the
  depth/normal buffer while the colour pass cuts them out — subtly broken
  lighting rather than an obvious error. You also lose early-Z on the dense ring.
  Measure it in the Grass tab before keeping it.

If the images do have transparency, the RGB *underneath* it is usually black, and
a plain box filter drags that black into the visible texels as mips descend. That
produced a near-black band at mid distance earlier this session. Use
`downsample_cutout()` — it normalises RGB by summed alpha — plus
`preserve_coverage()` so the blade doesn't thin out with distance. Both are in
`blade_atlas.rs` with tests.

---

## 6. Traps that cost real time this session

1. **WGSL compiles at runtime** via `naga_oil`. `cmake --build` succeeding proves
   nothing about shaders. You must launch and check `cwr.log`. A shader error
   silently falls the whole renderer back to GL33.

2. **Colour space.** The engine uploads *legacy 2001 game textures* as
   `Rgba8Unorm` — their stored values go straight into lighting because that era
   wasn't gamma-correct (see `textures.rs`, `terrain/mod.rs`). A **modern PNG is
   sRGB** and wants `Rgba8UnormSrgb`. Getting this backwards on the clump made it
   **3.6× too dark** and produced a black band. Match the format to the asset's
   provenance, not to the neighbouring code.

3. **Hue, not saturation.** The 2001 grass PAA measures (0.322, 0.375, 0.334) —
   blue *above* red, i.e. genuinely grey-teal. Boosting saturation pushes about
   the luma axis and *preserves* hue, so it made it worse. To recolour, drive a
   palette with the source's luminance instead.

4. **`packed` is taken.** All three vertex shaders already use `let packed` for
   vertex-index decode. A new `let packed = instances[i].packed` is a
   `redefinition` error at runtime. The existing code uses `inst_packed`.

5. **`WgrGrassParams` has a size `static_assert` on both sides** (currently
   **1632** = 102 × 16). Adding a field means updating `ffi.rs` *and*
   `wgpu_renderer.hpp` and keeping 16-byte alignment. It fails at compile time,
   which is the good case.

6. **Shape data is duplicated.** `species_shape()` in `grass.wgsl` returns
   (width scale, height scale, taper exponent) per species, and
   `grass_shadow.wgsl` re-implements it with hardcoded thresholds (`>= 6u`,
   `>= 4u`). Change one, change both, or shadows stop matching their blades.

7. **`LAYERS` is duplicated too** — `blade_atlas.rs` and `grass.wgsl` both
   declare 8, and the species byte is masked `& 7u`. Changing the layer count
   means both, plus the mask, plus `SPECIES_GRASS_END` / `SPECIES_WEED_END` in
   both files.

8. **Never fall back to the legacy PAA.** Anyone without the optional asset still
   has `Data.pbo`, so a PAA fallback hands *them* the grey-teal grass. The only
   fallback is the procedural path. Verified both ways this session.

---

## 7. Current state and measurements

Grass costs **1.147 ms** total (Grass tab → Benchmark, GRS-A). Measured split:

| | value |
|---|---|
| placement compute (near+mid+far) | 0.099 ms — **8.6%** |
| raster (prepass 0.335 + colour 0.546 + shadow 0.167) | 1.048 ms — **91%** |
| near instances / vertices | 43,667 / 2,620,020 (**85% of all grass verts**) |
| mid instances / vertices | 18,661 / 447,864 |
| far instances | 421 (a thin vestigial band) |

Two consequences worth inheriting:

- **Placement compute is not worth optimising.** Hierarchical cluster culling
  (proposed in the original plan) targets the 8.6%. Skip it.
- **Near is the only thing that matters for cost.** 2.62 M vertices, submitted
  three times (prepass, colour, shadow). Any near-LOD change should be A/B'd in
  the Grass tab against the 1.147 ms baseline.

### Known-good defaults

Detail radius 30 m, far radius 1 (a thin ~64–72 m band), mid photo clump cards
**on**, weed 12%, flower 5%, density noise scale 0.075 / strength 0.55.

### Open item

Near grass is still procedurally textured. Switching it to photo **clump** cards
is *not* a texture swap: near is placed at blade density (one instance per blade,
~10 cm apart), so clump cards there would stack ~20× the intended grass in
alpha-tested overdraw. It needs the placement density dropped ~20× at the same
time. Single-blade textures — the subject of this document — are the other route,
and keep the existing geometry and density untouched.
