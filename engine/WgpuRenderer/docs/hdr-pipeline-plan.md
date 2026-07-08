# Plan: HDR pipeline (linear-light offscreen target + exposure + tonemap)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** FINALIZED (2026-07-06). Design decisions locked (§0); implementation staged (§8).

> First item on the renderer roadmap (**HDR** → per-pixel → CSM → Forward+). Per-pixel sun +
> point lights landed ahead of it; HDR lands **before** Forward+, because clustered lighting exists
> to pile many lights on a fragment and without an HDR target they clip at 1.0. Independent of the
> object-rendering / GPU-culling plans (those are geometry, not shading).

---

## 0. Locked decisions (2026-07-06)

1. **Working color space: full linear-light.** Textures are decoded sRGB→linear on sample; lighting,
   fog, and blending happen in linear; the HDR target holds linear radiance; the tonemap resolve
   sRGB-encodes to the swapchain. This is a deliberate departure from today's gamma-naive pipeline
   (§1.1) and from the GL33 A/B look — it is a correctness overhaul and expects broad retuning.
2. **HDR target format: `Rgba16Float`.** Alpha kept (particles/blending), full precision, universally
   filterable/renderable. `Rg11b10Float` rejected (no alpha, banding risk in dark skies + night).
3. **Scope of the first PR: everything in this plan**, behind an HDR flag — offscreen target, Hable
   tonemap, clamp/`_accomodateEye` unwind, per-ToD manual exposure, GPU auto-exposure, and the NVG +
   scotopic night-eye display stage. Landed as ordered internal stages (§8), each individually
   buildable, so the tree never sits broken between stages.
4. **MSAA: single-sample now, MSAA-ready by construction.** No pass uses MSAA today (every attachment
   is `resolve_target: None`). The tonemap is a discrete fullscreen pass reading a **single-sample**
   HDR texture, so MSAA later = make the scene HDR target multisampled + resolve into that
   single-sample texture before tonemap. The real MSAA cost (every 3D/terrain pipeline's
   `multisample` state) is orthogonal to HDR and not blocked by it. See §4.7.
5. **Tonemapper: Hable (Uncharted 2 filmic)** first, AgX/LUT deferred behind a mode flag (§3).
6. **GL33 untouched** — stays LDR-direct-to-swapchain; the whole change is wgpu-internal (§6).

---

## 1. Motivation

Today the renderer shades **directly into the 8-bit swapchain** — 3D and 2D both target
`config.format` (sRGB suffix stripped, [lib.rs:124-130](../rust/src/lib.rs#L124-L130)). Everything
clips at 1.0 before it's ever tonemapped: bright sky, sun glint, and (soon) accumulated Forward+
lights blow out with no shoulder. An HDR pipeline renders lighting into a **floating-point offscreen
target** in linear light, applies **exposure** and a **tonemap curve**, and only then resolves to
the swapchain.

### 1.1 The pipeline is gamma-naive today (corrects the earlier stub framing)

**There is no linear working space and no "deferred sRGB encode" today.** Verified 2026-07-06:

- Albedo textures are all `*Unorm` — `Rgba8Unorm` / `Bc1-3RgbaUnorm`, no sRGB variant
  ([textures.rs:30-33](../rust/src/textures.rs#L30-L33)). `textureSample` returns the raw sRGB-encoded
  texel; nothing decodes it.
- Lighting and fog multiply/blend those gamma-encoded values directly and clamp to `[0,1]`
  ([shader3d.wgsl:216-244](../rust/src/gfx3d/shader3d.wgsl#L216-L244), terrain
  [terrain.wgsl:263-264](../rust/src/terrain/terrain.wgsl#L263-L264)).
- `remove_srgb_suffix` on the surface ([lib.rs:124-130](../rust/src/lib.rs#L124-L130)) does **not**
  defer an encode — it stops wgpu from applying a *second* linear→sRGB write to bytes that are already
  sRGB-encoded. The whole path is gamma-naive, exactly mirroring GL33.

Going linear therefore means work at **every color entry point**, not just a resolve pass — see §5.

## 2. Verified starting facts (2026-07-06)

- **No offscreen color target exists** — 3D (`Gfx3d::new(..., config.format, ...)`) and 2D both
  render straight to the surface format ([lib.rs:138-148](../rust/src/lib.rs#L138-L148)). Net-new.
- **The render loop is segmented, single color view.** `render_frame` acquires the swapchain,
  makes one `color` view, and replays the instancing plan as segments split at `ClearDepth`, all
  targeting that one view; a later overlay pass and 2D draws hit the same view
  ([lib.rs:249-423](../rust/src/lib.rs#L249-L423)). The HDR target replaces `color` for the 3D/terrain
  segments; 2D UI + overlay move **after** the tonemap onto the swapchain (§4.4).
- **Reversed-Z is unaffected.** Tonemapping is a fullscreen color operation; it never touches depth.
- **Lighting is a flat 256-light loop today** ([lighting.wgsl:29-65](../rust/src/shaders/lighting.wgsl#L29-L65)) —
  the thing that most wants HDR headroom once Forward+ raises the count.
- **No MSAA anywhere** (§0.4).
- GL33 is the default backend and stays LDR-direct-to-swapchain — untouched (§6).

### 2.1 Existing LDR compensation to unwind (verified 2026-07-06)

There is **no real eye adaptation today** — the misleadingly-named "accommodation" is a static gain,
and everything is pre-scaled + clamped to `[0,1]` in shading. Four pieces to deal with:

- **`Engine::_accomodateEye`** ([Engine.cpp:316-328](../../Poseidon/Graphics/Core/Engine.cpp#L316-L328))
  is **not** eye adaptation — it's `HWhite * _usrBrightness` (default 1.6), with NVG hijacking it to
  `Color(0, 8.0, 0)`. No scene luminance, no temporal state. Baked into ~20 per-material/per-light
  multiplies via `GetAccomodateEye()` ([LandscapeRender.cpp:1104](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1104),
  [Material.cpp:93-96](../../Poseidon/Graphics/Rendering/Lighting/Material.cpp#L93-L96),
  [Object.cpp:1185](../../Poseidon/World/Scene/Object.cpp#L1185), [Scene.cpp:497](../../Poseidon/World/Scene/Scene.cpp#L497), …).
  **Action:** reduce to identity under the HDR flag; the brightness knob moves to a post-tonemap
  output gain and the NVG hijack becomes a display-stage mode (§4.5).
- **`NightEffect`** ([Lights.cpp:155-222](../../Poseidon/Graphics/Rendering/Lighting/Lights.cpp#L155-L222)) —
  sun-elevation day/night model (0=day…1=night). It scales *artificial* lights
  ([TransLight.cpp:440-451](../../Poseidon/Graphics/Rendering/Lighting/TransLight.cpp#L440-L451)) and is the
  generic gameplay "is it night" signal (shadow fade, AI vis, headlights). **Keep it as the gameplay/
  lights signal; stop using it as a brightness multiply** — darkness comes from true intensities +
  exposure, not a pre-scale. (Shaders currently receive `diffuse*nightEffect` / `ambient*nightEffect`,
  packed on the CPU into `WgrLight` — [ffi.rs:184-191](../rust/src/ffi.rs#L184-L191),
  [EngineWgpu.cpp:649-657](../EngineWgpu.cpp#L649-L657).)
- **The clamps are the actual no-HDR hack:** `min(...,1.0)` in
  [lighting.wgsl:14](../rust/src/shaders/lighting.wgsl#L14), `clamp(...,0,1)` in
  [shader3d.wgsl:219](../rust/src/gfx3d/shader3d.wgsl#L219),[:231](../rust/src/gfx3d/shader3d.wgsl#L231),
  plus `SaturateMinMax()` / `PackedColor255` at CPU stages
  ([Lights.cpp:261](../../Poseidon/Graphics/Rendering/Lighting/Lights.cpp#L261),
  [Scene.cpp:498](../../Poseidon/World/Scene/Scene.cpp#L498),
  [TransLight.cpp:417-425](../../Poseidon/Graphics/Rendering/Lighting/TransLight.cpp#L417-L425)).
  **Action:** relax so linear radiance can exceed 1.0 into the HDR target.
- **NVG + scotopic "night eye"** (Purkinje blue/green desaturation,
  [LandscapeRender.cpp:1519](../../Poseidon/World/Terrain/LandscapeRender.cpp#L1519) → GL33 pixel shader)
  are **display-space** color ops. The wgpu backend doesn't implement night-eye at all
  (`Engine::EnableNightEye` is a base no-op) — net-new here. **Action:** re-home both into the
  tonemap/display stage rather than baking into per-material colors (§4.5).

## 3. Tonemapper decision

**Start with Hable (Uncharted 2 filmic).** A per-channel 1D filmic curve — ~10 lines of shader,
moderately saturated out of the box, tunable, and the exact curve ARMA 3 shipped after moving off
Reinhard, so it's known-good for this content lineage. In full-linear mode the curve operates in its
correct domain (linear radiance in, tonemapped-then-sRGB-encoded out).

**Future improvement (deferred): AgX + a "Punchy" look, via a 3D LUT.** Better highlight robustness
(path-to-white, no hue skew) and superior on gray uniforms / overcast / night, but base AgX is muted
→ needs a saturation/contrast look, ideally per time-of-day, and it's more plumbing (matrix inset →
log2 → sigmoid → outset → look, usually baked to a 3D LUT). Structure the tonemap step behind a
`tonemap_mode` override so AgX slots in later without reworking the pass. Do **not** implement AgX
now. (Avoid ACES entirely — hue skews on foliage/sky.)

## 4. Pass architecture

Frame order under the HDR flag (`render_frame`, wgpu-side):

```
  shadow cascades        ──► depth arrays        (unchanged)
  terrain sun-shadow mask ─► mask texture         (unchanged)
  3D/terrain segments    ──► HDR target (Rgba16Float, linear)   [was: swapchain]
  auto-exposure compute  ──► adapted-luminance buffer (persistent, GPU-only)  §4.2
  tonemap resolve        ──► swapchain            §4.3  (HDR → exposure → NVG/night-eye → Hable → sRGB)
  2D / UI                ──► swapchain            §4.4  (gamma-space, composited after tonemap)
  dev overlay            ──► swapchain            (moves after tonemap with 2D)
```

### 4.1 Offscreen HDR color target
- `Rgba16Float`, allocated in `ensure_depth`'s sibling (a new `ensure_hdr`) and resized with the
  swapchain. Single-sample now (§4.7). Cleared to the (linearized, §5) clear color on the first
  segment; `Load` on subsequent segments — same segment logic as today, just a different view.
- 3D and terrain pipelines are rebuilt against `Rgba16Float` instead of `config.format`. This is a
  per-pipeline `targets[0].format` change; blend states are unchanged (alpha blending in linear is
  the intended behavior now).
- The depth target and all shadow passes are untouched.

### 4.2 Exposure / eye-adaptation (first-class subsystem — the real day/night workhorse)
- The curve is the shoulder; **exposure decides where the scene sits on it.** A sim's noon→night
  luminance range makes this matter more than the curve choice. This is the first eye adaptation the
  engine has ever had (§2.1) — a strict improvement, especially at night.
- **Manual per-time-of-day exposure keys first** — deterministic, plumbed from the same sun/ToD model
  that drives `NightEffect`, delivered as a scalar in `WgrFrame`/the frame UBO. Replaces the removed
  `_accomodateEye` gain.
- **GPU auto-exposure as the same-PR follow-up, NO CPU readback.** A compute pass histograms the HDR
  target → writes an adapted-luminance value into a small **persistent GPU buffer** → smoothed
  against last frame on the GPU → sampled by the tonemap resolve. Readback would reintroduce the
  GPU→CPU stall the HDR pass exists to avoid.
- **Exposure is client-visual-only and decoupled from gameplay.** AI visibility / shadow fade /
  lights-on key off `NightEffect`, not exposure, so auto-exposure need not be deterministic and can't
  create an unfair adaptation advantage.
- **Genre constraint:** mil-sim players dislike aggressive auto-exposure (it reveals concealed units
  in shadow, fights NVG). Constrain it — tight min/max limits, slow adaptation rate, anchored to the
  per-ToD key rather than free-running. The auto term modulates the manual key, it doesn't replace it.

### 4.3 Tonemap resolve pass (Hable)
- Fullscreen triangle: sample HDR target → multiply by exposure (manual key × clamped auto term) →
  apply the display stage (NVG / night-eye, §4.5) → Hable curve → linear→sRGB encode → write
  swapchain. No depth.
- Hable shoulder/toe/white params + `tonemap_mode` + output brightness gain exposed as uniforms /
  pipeline-overridable constants for tuning and the future AgX seam.

### 4.4 2D / UI compositing order — IMPLEMENTED (2026-07-06)

Landed via a **scene→UI resolve marker**. The engine emits `Engine::ResolveSceneToDisplay()` at the
scene/UI seam ([World.cpp:1569](../../Poseidon/World/World.cpp#L1569), right after `PhaseDrawPost`,
before the HUD block) — `EngineWgpu` turns it into `WGR_CMD_RESOLVE` (→ `Plan3dOp::Resolve`). The
renderer replays in two phases: **scene** draws (terrain, objects, sky, rain, cockpit — everything
before the marker) render into the HDR target; at the marker the tonemap resolves to the swapchain;
the **UI phase** (HUD, map, menus, dialogs, cursor, and 3D-in-UI previews) draws **display-referred
straight to the swapchain** (gfx2d gains a swapchain-format `pipelines_display` set, selected by a
`display` flag on `draw_one`). GL33 / LDR-direct: the marker is a no-op.

Key facts that forced this design (verified 2026-07-06): draw **method** doesn't classify scene vs UI
— sky/horizon and 3D-in-UI both take the software `ClipUser0` path, and rain uses `Draw2D` but in the
scene phase. Only the **frame phase** (before/after the seam) separates them, and the seam can't be
`Clear(depth)` because vehicle interiors depth-clear mid-scene ([TransportCore.cpp:1877]). 3D-in-UI
(notebook/options/editor preview) is CPU-projected to screen space (SW T&L → 2D-with-depth), so it
composites correctly in the display phase with no tonemap — matching GL33.

Historical note (superseded):
- UI is authored for gamma-space sRGB → composite **after** tonemap, drawing 2D straight to the
  swapchain post-resolve, so UI isn't tonemapped/exposure-scaled and its `*Unorm` textures need no
  linear decode. The dev overlay moves with it. In the segmented loop, 2D draws that today interleave
  with 3D inside a segment (`Plan3dOp::Draw2D`) must be re-examined: interleaved 2D that is genuinely
  *in-world* (rare) stays in the HDR segment and gets linearized; true screen-space UI moves to the
  post-tonemap swapchain pass. Audit `Plan3dOp::Draw2D` usage during Stage 3.

### 4.5 NVG + scotopic "night eye" → display stage
- Both are display-space color operations, folded into the tonemap resolve (before the Hable curve,
  after exposure):
  - **NVG** = green-monochrome + gain, replacing the `_accomodateEye = (0,8,0)` hijack. A display-stage
    mode flag from the engine (the same signal that set the hijack).
  - **Night-eye** = Purkinje blue/green desaturation (`rgbEyeCoef`), driven by
    `nightEye = (1 - sunDiffuse.R) * NightEffect`. Implement on wgpu (net-new;
    `Engine::EnableNightEye` is a base no-op today). NVG suppresses night-eye (matches GL33).
- These need two new scalars/flags in the frame UBO (nvg mode, nightEye coefficient).

### 4.6 Bloom / post (optional, later — out of scope)
- HDR unlocks physically-based bloom (threshold in linear HDR before tonemap). Not in this PR; the
  insertion point is **pre-tonemap** (between §4.2 and §4.3) so the pass order already leaves room.

### 4.7 MSAA-readiness (later, out of scope for this PR)
- Adding MSAA later: allocate the HDR target with `sample_count = N` + a single-sample resolve
  texture; set every 3D/terrain/shadow-consuming pipeline's `multisample.count = N`; resolve the
  multisampled HDR into the single-sample HDR before the tonemap pass (tonemap reads the resolved
  single-sample texture, unchanged). Keeping the tonemap on a single-sample input now is what makes
  this a localized change. Depth becomes multisampled too. No HDR design element blocks it.

## 5. Linear-conversion strategy (the core correctness work)

Every value that enters the linear HDR domain must be sRGB→linear decoded exactly once. The entry
points and where each is handled:

- **Albedo textures → decode in the fragment shader.** Shared `*Unorm` textures are also sampled by
  the gamma-space 2D path, so do **not** swap to sRGB texture views (would double-decode 2D or force
  dual views on shared BC textures). Add a `srgb_to_linear(vec3)` helper (proper piecewise sRGB, not a
  bare `pow(x,2.2)`) in a shared module and apply it to `base.rgb` right after `textureSample` in
  `fs_main` ([shader3d.wgsl:192](../rust/src/gfx3d/shader3d.wgsl#L192)) and the terrain fragment shader.
  Alpha is linear already — never decode `.a`.
- **CPU-supplied colors → linearize at their source in `EngineWgpu`** before packing into
  `WgrDraw3D` / `WgrCamera` / `WgrLight` / `WgrFrame`: clear color, `fog_color`, sun diffuse/ambient,
  per-light diffuse/ambient, and the material terms (emissive, sun/light diffuse/ambient, specular).
- **⚠ Color-fold subtlety (load-bearing).** Several material terms are **pre-folded on the CPU** as a
  gamma-space product, e.g. `mat_sun_diffuse = rawSunColor × materialDiffuse`
  ([ffi.rs:150-167](../rust/src/ffi.rs#L150-L167)). `linear(a·b) ≠ linear(a)·linear(b)`, so decoding
  the *folded* product is wrong. For correctness, **linearize the two source colors before the fold**
  in the packing code (decode `rawSunColor` and `materialDiffuse` separately, then multiply). Where a
  clean split is impractical, decoding the folded product is the documented pragmatic fallback — a
  mild saturation/brightness shift that the retune absorbs — but the split is preferred and is the
  reason this touches C++ lighting broadly rather than being a shader-only change.
- **Light/material scalars that are not colors** (N.L, attenuation, cone, shadow strength, fog factor)
  stay as-is — they're linear multipliers and space-agnostic.
- **Remove the `[0,1]` clamps** (§2.1) so linear radiance exceeds 1.0 into the target.

All of the above is **gated on the HDR flag** so GL33 and the flat-LDR wgpu fallback keep the exact
gamma-naive behavior (§6).

## 6. GL33 / non-regression + the flag

Entirely wgpu-internal. A single **HDR flag** gates the whole path:
- **Rust side:** the flag rides in `WgrSurfaceDesc` (or a new init field) so pipeline formats + the
  presence of the tonemap pass are decided at renderer construction. Off = today's exact
  direct-to-swapchain, gamma-naive path (kept as the A/B reference and fallback).
- **C++ side:** `EngineWgpu` reads the flag (config/CLI, mirroring `--render`'s plumbing in
  `GraphicsEngineFactory`) and, when on, (a) linearizes source colors + reduces `_accomodateEye` to
  identity, (b) routes NVG/night-eye to the display stage instead of the color bake.
- GL33 keeps rendering LDR-direct-to-swapchain, completely unchanged.

## 6.5 First-light feedback (2026-07-06, user ran WGR_HDR=1)

The linear scene works; it needs balancing (a tuning journey, not a bug list):
- **Nights are correctly dark — too dark to play** as-is; eye adaptation (§4.2 auto-exposure) is
  expected to lift them. Manual per-ToD exposure (stage 3) is the near-term lever.
- **Daytime wants more overall brightness**; **dusk/dawn shadowed areas want lifting** too. This is
  exposure + the `_accomodateEye`-→-identity retune (stage 2 C++) + the per-ToD look.
- **Night sky too bright** — an *asset* issue, to be fixed by the future **procedural sky**, not here.
- **Moon emits no light/shadow** — a nice future improvement (a second, dim directional light +
  its own shadow term), out of scope for HDR.
- **Town/city light entities are ~10× too bright with huge radius and almost no falloff** — should
  have smooth falloff (confirmed against GL33). A lighting bug to fix separately (the `WgrLight`
  attenuation / radius the point-light gather feeds), not a tonemap issue.
- **UI is tonemapped (wrong)** — confirmed. Applies to screen-space 2D *and* to **in-world 3D UI**
  (the main-menu laptop) to a lesser extent. The 3D-UI case means the §4.4 audit can't just hoist all
  2D — 3D UI panels rendered in-world also need a "not scene radiance" path (e.g. an emissive/unlit
  material flag that the tonemap treats as display-referred, or drawing them post-resolve).

### 6.6 Look-tuning workflow (IMPLEMENTED — fixed curve + exposure/grading model)

**Decision (2026-07-06):** the tonemap **curve is fixed** (one Hable "film stock"); the per-ToD look
is **exposure + a colour-grade block**, matching how real engines decompose it (a curve is a display
transform you pick once; exposure/eye-adaptation is the dynamic lever; warm-hour colour comes from the
sun/sky/fog lighting; grading = the artistic layer, blended per-ToD). This replaced an earlier
approach that interpolated the raw Hable A–F constants per hour — that conflated exposure/contrast
into the film curve and doesn't generalise. See the AgX note in §3.

Implemented:
- **`WgrTonemap` (12 f32, 48 B)** = `exposure`, `mode`(hable/passthrough), `encode`, and the grade:
  `temperature`, `tint`, `contrast`, `saturation`, `lift` (shadow), `gain`. Shader order
  ([tonemap.wgsl](../rust/src/tonemap.wgsl)): exposure → white balance → **fixed Hable** → contrast →
  saturation → shadow-lift → gain → clamp → sRGB. Pushed live via `wgr_set_tonemap`.
- **ImGui "Tonemap" tab** ([DebugOverlay.cpp `DrawTonemapTab`](../../Poseidon/Dev/Debug/DebugOverlay.cpp))
  with an **Auto (time of day)** checkbox, exposure + grade sliders (read-only in auto), a ToD
  readout, and **copy-preset-to-clipboard**. `Engine::TonemapSettings` + `Get/SetTonemapSettings` +
  `Get/SetTonemapAuto` virtuals (appended at class end — vtable-safe); `EngineWgpu` implements.
- **Per-ToD preset keyframes** (`kTonemapPresets` in `EngineWgpu.cpp`), interpolated by
  `Glob.clock.GetTimeOfDay()` in `UpdateAutoTonemap` (called from `NextFrame`), clamped outside the
  keyed range. Seeded from the user's 3 captured presets (04:30 / 12:00 / 16:30) — exposure + gain
  only; grade starts neutral, to be re-tuned via the tab. A `WGR_*` env override flips to manual.

Still open: **night/dusk keyframes** (need eye-adaptation + procedural sky first); the grade values
are a first pass to re-tune by eye; auto-exposure (Stage 5) will subsume most exposure keying.

## 7. Open questions (remaining)

- Auto-exposure adaptation curve/limits tuning for the genre constraint (§4.2) — needs in-engine
  tuning once the histogram pass is live; ship conservative defaults.
- Per-ToD exposure key values — where the ToD/sky model exposes the scalar and the day curve's shape;
  co-tune with the removed `_accomodateEye` brightness so overall daytime brightness is preserved.
- Whether any `Plan3dOp::Draw2D` interleaved draw is genuinely in-world vs screen-space UI (§4.4) —
  audit during Stage 3.
- Later: per-ToD AgX look as LUTs — how many, how they blend across the day.

## 8. Implementation stages (all in this PR, each independently buildable)

1. **HDR target + tonemap plumbing.** ✅ **LANDED.** `Rgba16Float` target (`ensure_hdr` + resize),
   3D/terrain/2D pipelines rebuilt against it, `tonemap.rs`/`tonemap.wgsl` fullscreen resolve to the
   swapchain, dev overlay after the resolve. Gated by `WGR_HDR` (env for now — the engine config/CLI
   flag is wired in stage 2's C++ work, §6). Swapchain stays `Unorm`; the resolve encodes manually.
2. **Linear conversion.** ✅ **SHADER PATH LANDED.** `shaders/color.wgsl` `srgb_to_linear`; albedo +
   folded material/sun/light/fog colours decoded in `shader3d.wgsl` / `terrain.wgsl` / `lighting.wgsl`,
   gated by a `linear` pipeline-override (driven off the `Rgba16Float` scene format); `[0,1]` clamps
   dropped under `linear`; CPU clear colour linearized; tonemap defaults flipped to Hable + sRGB
   encode. Used the §5 **pragmatic in-shader fold** (decode the CPU-folded products) to keep it
   Rust-only + fully behind the flag. **REMAINING (C++, needs visual verify):** reduce `_accomodateEye`
   to identity under the flag; optionally upgrade to the §5 "correct split" (linearize source colours
   before the fold in `EngineWgpu`); co-tune exposure so daytime ≈ the GL33 reference.
   **KNOWN-INTERIM:** interleaved 2D (incl. screen-space UI) renders into the HDR target and is
   tonemapped → looks wrong under `WGR_HDR=1` until stage 3's §4.4 audit.
3. **Manual per-ToD exposure.** Plumb the exposure scalar from the sun/ToD model through `WgrFrame`
   into the resolve; audit `Plan3dOp::Draw2D` (§4.4) and re-home screen-space UI past the tonemap.
4. **NVG + night-eye display stage.** Add nvg-mode + nightEye scalars to the frame UBO; implement both
   in the resolve pass; route `Engine::EnableNightEye` + the NVG path to them.
5. **GPU auto-exposure.** Histogram compute pass over the HDR target → persistent adapted-luminance
   buffer → GPU smoothing → sampled by the resolve, constrained per §4.2. No readback.

### 8.1 Dev toggles (current)
- `WGR_HDR=1` — enable the HDR path (offscreen target + tonemap). Default off = LDR-direct A/B ref.
- `WGR_TONEMAP` — 1 = Hable (default when HDR on), 0 = passthrough clamp (plumbing check).
- `WGR_HDR_ENCODE` — 1 = linear→sRGB encode (default when HDR on), 0 = write as-is.
- `WGR_EXPOSURE` — linear exposure scale before the curve (default 1.0).

## 9. Cross-references
- [forward-plus-plan.md](forward-plus-plan.md) — the main beneficiary; HDR lands first.
- [rendering-performance-plan.md](rendering-performance-plan.md) — umbrella perf/roadmap.
