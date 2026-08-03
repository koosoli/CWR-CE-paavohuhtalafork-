# Plan: Cascaded shadow maps — range, quality, and multiview rendering

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`); shared shadow math in
`engine/Poseidon/Graphics/Shadow/ShadowMath.*` (pure, unit-tested, ported 1:1 into `shadow.wgsl`).
**Status:** PLANNED (2026-07-08). Tier 1 is independent and lands now; Tiers 2–3 have roadmap edges.
**Roadmap slot:** consumes Phase 2 (depth prepass) and Phase 4 (GPU-driven multi-view cull). See
[implementation-roadmap.md](implementation-roadmap.md). Tier 1 needs neither and can land immediately.

> **RND-030 reconciliation (2026-08-03):** the status line above is out of date, but less so than the
> other reconciled plans, so this note is per-item rather than a blanket "implemented".
>
> A cascaded shadow map system is live in the branch — `wgr_shadow_cascades` (`lib.rs:1884`),
> `gfx3d/shadow_depth.wgsl`, `gfx3d/gpu_driven_shadow.wgsl` — but most of that predates this plan.
> Checking **Tier 1** item by item against the branch:
>
> | Tier 1 item | State |
> | --- | --- |
> | 1. Decouple shadow distance from the 250 m clamp | **Landed.** `shadowDistance` defaults to 400 m (`Engine.hpp:826`) and the dev panel exposes it as "explicit cascade reach, decoupled from the 250 m clamp". |
> | 2. `MAX_CASCADES` 4 → 8 at 2048² | **Not landed.** Still `const MAX_CASCADES: u32 = 4` (`gfx3d/mod.rs:42`), and the dev panel's Cascades slider is capped at 4. |
> | 3. Retune `splitCoef` / `distanceCoef` and expose them | **Landed.** Both are sliders in the dev overlay (`DebugOverlay.cpp:2755`, `:2761`). |
> | 4. Closed-vs-single-sided caster split with front-face culling | **No evidence found** in `gfx3d/mod.rs` or `gfx3d/cull.rs`. |
>
> Tiers 2 and 3 were not audited. Note that the reconciliation report cites `MAX_CASCADES = 4` as
> evidence this plan shipped; that is the value item 2 exists to *change*, so it is evidence of the
> opposite. The report's row for this plan is corrected there.
>
> The status line is left as written rather than rewritten, so the document's own history stays
> readable. See [RND-030-renderer-plan-reconciliation-20260802.md](../../../docs/roadmap/decisions/RND-030-renderer-plan-reconciliation-20260802.md).

> **RND-030 reconciliation (2026-08-02):** the status line above is out of date: cascaded shadow maps are **implemented and live** in this branch. `MAX_CASCADES = 4` (`gfx3d/mod.rs:42`), a `wgr_shadow_cascades` pass (`lib.rs:1874`), `shadow_depth.wgsl` and `gpu_driven_shadow.wgsl`, plus far-cascade caster handling in `gfx3d/cull.rs:1392`.
>
> The status line is left as written rather than rewritten, so the document's own history stays readable. See [RND-030-renderer-plan-reconciliation-20260802.md](../../../docs/roadmap/decisions/RND-030-renderer-plan-reconciliation-20260802.md).

> **The ask.** Current shadows are sharp for ~7 m, then mush, and hard-cap at 250 m. Push usable object
> shadows to **several hundred metres → ~1 km** for an open-world game, and fix the "low quality after
> the first cascade", light-leak/peter-panning, and short-range complaints. Terrain-on-terrain is already
> covered well by the raymarched horizon mask ([terrain_shadow.wgsl](../rust/src/terrain/terrain_shadow.wgsl));
> CSM's job is **objects + near terrain**, so we frame the range push as "objects to mid-range, terrain
> self-shadow to the horizon" and do not spend cascade texels on terrain the raymarch handles better.

---

## Implementation order (read this first)

**This plan is *not* gated on the depth prepass except for one refinement.** The bulk — and the biggest
quality win — has no dependency on any other planned work and should land first.

| Piece | Real gate | When |
|---|---|---|
| **Tier 1** — decouple range clamp, `MAX_CASCADES` 4→8 @2K, split retune, closed-caster front-face cull | **none** | **now — before the prepass.** Pure retune + contained edits; the headline fix (50–100 m mush → several hundred metres even) and the highest quality-per-hour in the plan. |
| Multiview **render** path (1..N views/pass, backend cap, union-list + cascade-bitmask VS collapse) | Phase 4 GPU multi-view cull — *soft* (throwaway-avoidance, not correctness) | **with Phase 4.** The cull emits the union list + per-instance cascade mask multiview wants; building a CPU-side union builder first is work Phase 4 discards. |
| SDSM-lite tight fit (z/far tightening) + `Depth16Unorm` far cascades | **Phase 2 depth prepass — hard** (needs scene min/max depth) | after the prepass. |
| Tier 3 — Poisson wide PCF, EVSM/MSM far cascades, static shadow atlas, PCSS | **none** | whenever. |

So: the prepass gates only the *tight-fit polish* (SDSM-lite, decision 14); the contact-shadow synergy
(decision 15) is soft. The stronger scheduling gravity is toward **Phase 4** — for the multiview render
path, to avoid throwaway — not Phase 2. **Do not hold Tier 1 for anything.**

---

## 0. Where we are today (verified 2026-07-08)

**Pipeline shape.** Per-frame cascade matrices/splits are built CPU-side by
`BuildShadowCascadesTiered` ([ShadowMath.cpp:750](../../Poseidon/Graphics/Shadow/ShadowMath.cpp#L750)),
wired in [SceneShadowPass.cpp:77](../../Poseidon/World/Scene/SceneShadowPass.cpp#L77), uploaded as a
`WgrShadowPass`, and sampled in [shadow.wgsl](../rust/src/shaders/shadow.wgsl) (shared by the lit-mesh and
terrain pipelines so they self-shadow identically). Depth pass is one array layer per cascade,
depth-only, alpha variants discard cutout foliage ([shadow_depth.wgsl](../rust/src/gfx3d/shadow_depth.wgsl)).

**The five root causes of the current look:**

1. **Hard range cap at 250 m.** `bp.farD = ENGINE_CONFIG.shadowsZ`
   ([SceneShadowPass.cpp:96](../../Poseidon/World/Scene/SceneShadowPass.cpp#L96)), and `shadowsZ` is
   clamped `1..250` at load ([WorldImpl.cpp:1751](../../Poseidon/World/WorldImpl.cpp#L1751), default 250,
   [WorldImpl.cpp:1205](../../Poseidon/World/WorldImpl.cpp#L1205)). The legacy view-distance slider is the
   ceiling — **there is no way to reach a kilometre today.**

2. **The splits crush 3 cascades into the first ~50 m.** With `splitCoef = 0.90` (near-fully logarithmic,
   [Engine.hpp:782](../../Poseidon/Graphics/Core/Engine.hpp#L782)) over `[near, 250]`, the 4 cascades land
   at ≈ **[0.1–7 m], [7–17 m], [17–51 m], [51–250 m]**. Cascade 0 is razor-sharp over 7 m; the entire
   51–250 m band is one map. That *is* "great near, mush after cascade 0".

3. **Bounding-sphere fit wastes ~half the texels.** `FrustumBoundingSphere`
   ([ShadowMath.cpp:499](../../Poseidon/Graphics/Shadow/ShadowMath.cpp#L499)) → square ortho AABB. For a
   long far slice the radius is dominated by slice *length*, so the map is hundreds of metres square while
   the visible cross-section is a thin wedge. It is rotation-invariant (kills swim — deliberate,
   `QuantizeShadowRadius` + double-precision world snap in `CascadeLightVPStable`), but expensive in texel
   budget exactly where we are already texel-starved.

4. **Peter-panning is partly a wgpu-specific bias bug.** Tuning says "biasBase small — front-face culling
   does the acne work" ([Engine.hpp:783](../../Poseidon/Graphics/Core/Engine.hpp#L783)), but the wgpu
   shadow pipeline runs **`cull_mode: None`** ([gfx3d/mod.rs:1529](../rust/src/gfx3d/mod.rs#L1529)) and
   leans entirely on `DepthBiasState { constant: 4, slope_scale: 2.5 }`
   ([gfx3d/mod.rs:1537](../rust/src/gfx3d/mod.rs#L1537)) + an in-shader normal offset that **scales with
   texel size** ([shadow.wgsl:81](../rust/src/shaders/shadow.wgsl#L81)). A ~1 m far-cascade texel pushes
   the receiver ~2 m off its normal → visible peter-panning at distance.

5. **Filtering is a fixed 3×3 tent.** Plane-corrected HW-PCF, `textureSampleCompareLevel`
   ([shadow.wgsl:113-129](../rust/src/shaders/shadow.wgsl#L113)). Good near; nothing hides coarse far
   texels.

**Defaults** ([Engine.hpp:776](../../Poseidon/Graphics/Core/Engine.hpp#L776)): 4 cascades, resolution
2048, `distanceCoef 1.0`, `splitCoef 0.90`, `omniCount 0` (pure frustum slices), `fadeRange 40`,
`biasBase 0.00002`, `pcf 1.0`, `normalOffset 1.0`.

**Format / memory.** `SHADOW_FORMAT = Depth32Float` ([gfx3d/mod.rs:18](../rust/src/gfx3d/mod.rs#L18)),
single `D2Array`, `MAX_CASCADES = 4` ([gfx3d/mod.rs:19](../rust/src/gfx3d/mod.rs#L19)). **4K D32 = 64
MiB/layer** (16.7 M texels × 4 B). Forward-Z (clear 1.0, `LessEqual`) — correct for ortho (linear depth;
reversed-Z buys nothing here), do **not** convert.

**LOD.** The GPU caster path already selects **one `geomLOD` per object** and submits it once
([SceneShadowPass.cpp:492](../../Poseidon/World/Scene/SceneShadowPass.cpp#L492),
[:536](../../Poseidon/World/Scene/SceneShadowPass.cpp#L536)); the backend buckets that one mesh into every
cascade it overlaps via `cascade_mask` ([gfx3d/mod.rs:1607](../rust/src/gfx3d/mod.rs#L1607)). One-LOD-per-
object-across-cascades is the **status quo**, not something multiview introduces.

**Coupling to watch.** The volumetric fog froxel samples these cascade matrices for near-field occlusion
([lib.rs:591-593](../rust/src/lib.rs#L591)). Any change to cascade **count/layout ripples into the froxel
shader** — the `ShadowBlock` UBO (`cascade_vp: array<mat4x4,4>`, `splits: vec4`,
[frame.wgsl:15-24](../rust/src/shaders/frame.wgsl#L15)) is shared. Budget for it.

---

## 1. Design decisions

1. **Decouple shadow distance from the 250 m legacy clamp.** Add a wgpu-path shadow-distance not bound by
   the `shadowsZ` serialize clamp; this is the literal prerequisite for a km. But range must arrive *with*
   the distribution fix (decisions 2–3) or it just smears further.

2. **Raise `MAX_CASCADES` 4 → 8, at 2048² not 4096².** Density = `cascade_extent / resolution`; more
   cascades cover smaller extents, so density rises *even at lower resolution*. **8×2048² = 128 MiB beats
   4×4096² = 256 MiB on quality at half the memory** (33.5 M vs 67 M texels, but distributed instead of
   dumping 50–250 m in one map). Do **not** ship 8×4096² (512 MiB — 6–8 full 4K PBR sets; way too much for
   one feature). Touch points: `MAX_CASCADES` ([gfx3d/mod.rs:19](../rust/src/gfx3d/mod.rs#L19)),
   `ShadowBlock` arrays (`cascade_vp` → 8, `splits: vec4` → `array<vec4,2>` or `array<f32,8>`,
   [frame.wgsl:15](../rust/src/shaders/frame.wgsl#L15)), the `for i in 0..4` loops in
   [shadow.wgsl:23,56](../rust/src/shaders/shadow.wgsl#L23), the `[4]` arrays in `CascadeSet`
   ([ShadowMath.hpp:222](../../Poseidon/Graphics/Shadow/ShadowMath.hpp#L222)), the per-cascade
   `ShadowPassUbo` dynamic-offset count, and `target.layers`.

3. **Retune the split scheme for the new range.** `splitCoef 0.90` over 1 km is pathological — move toward
   **~0.7–0.8** so cascades spread, and reconsider `distanceCoef` (currently 1.0). Cheap, dev-panel-
   tunable, largest perceived-quality delta per hour of work.

4. **Fix the bias/peter-pan mismatch: split casters by closedness.** Render **closed solids with
   `cull_mode: Front`** (render back faces — removes acne with almost no depth bias, kills peter-panning),
   keep single-sided walls/roofs on `cull_mode: None` + bias. The classifier already distinguishes caster
   kinds ([ClassifyShadowCaster](../../Poseidon/Graphics/Shadow/ShadowMath.cpp#L844)); add a "closed" bit
   and a second pipeline. Improving texel density (2) also shrinks the texel-scaled normal offset
   automatically, so peter-panning at distance recedes on its own.

5. **Render cascades with multiview by default; one parameterized 1..N path; Metal fallback later.**
   wgpu 29.0.4 supports multiview on Vulkan/Metal/DX12 (reworked in v28: view instancing on DX12, vertex
   amplification on Metal). The renderer issues cascades as **`ceil(cascade_count / views_per_pass)`**
   multiview passes, where `views_per_pass = min(cascade_count, backend_cap)`. This is *one* code path;
   `views_per_pass = 1` degrades cleanly to per-view rendering on any backend without the feature. No
   separate fallback renderer.

6. **Per-backend view-count cap is a hardcoded table keyed on `adapter.get_info().backend`.** wgpu 29
   does **not** expose a `max_multiview_view_count` Limit — the ceiling is API-defined and must be applied
   by us, **clamped before pipeline creation** (over-cap on DX12 fails at *pipeline creation*, not with a
   friendly validation error):

   | Backend | Cap | 8 cascades → | Source of cap |
   |---|---|---|---|
   | Vulkan | 8+ (query `maxMultiviewViewCount`; spec floor **6**, NV=32, Intel 16+, AMD ~8) | 1 pass | `VkPhysicalDeviceMultiviewProperties` |
   | DX12 | **4 (hard)** | 2 passes | `D3D12_MAX_VIEW_INSTANCE_COUNT` = 4, all tiers |
   | Metal (later) | vertex amplification count (typically **2**; query `maxVertexAmplificationCount`) | 4 passes | Apple GPU limit |
   | GL / no feature | 1 | 8 passes | — |

   Query the Vulkan limit at init and clamp `cascade_count` to it so a conformant 6-view device degrades
   to 2 passes rather than failing validation.

7. **Group contiguous cascades with per-pass `base_array_layer` views — not `SELECTIVE_MULTIVIEW`.** For
   DX12's two groups (or Metal's four), create a texture **view with `base_array_layer` offset** per pass
   and use a plain low-bits mask against it; the shader's view index stays 0-based per pass and we add the
   group's base offset to pick the cascade matrix. This keeps us off the optional `SELECTIVE_MULTIVIEW`
   feature (only needed for *non-adjacent* layer masks, which we never want).

8. **Multiview consumes a union list + per-instance cascade bitmask; the VS collapses non-members.** The
   `multiview_mask` is per-**pass**, not per-draw — every caster in a pass is VS-invoked for *all* views
   in that pass. So the GPU cull emits the **union** of casters (visible in *any* cascade of the group)
   plus a per-instance cascade mask; the vertex shader tests `bit(view_index + base)` and **collapses to a
   degenerate triangle** for cascades the caster doesn't belong to. This is a known, shipped pattern.

9. **Accept one LOD per object across a pass's cascades — it is already the status quo and it is correct.**
   An object is only rendered into the 1–2 *adjacent* cascades whose ortho box contains it (via the
   cascade mask); those have near-identical texel density, so there is no LOD mismatch to exploit.
   Far-object coarsening already comes from **per-object distance LOD** (`casterLodBias`,
   [SceneShadowPass.cpp:497](../../Poseidon/World/Scene/SceneShadowPass.cpp#L497)), not from per-cascade
   LOD. Per-cascade LOD would only help a single large object straddling many bands (one mesh anyway).

10. **The real multiview tax is VS invocations on the union, not detail level.** A near object living only
    in cascade 0 still pays ~8× VS invocation + index fetch (7 wasted, collapsed) in the Vulkan single-
    pass config. Mitigations already in hand: DX12's forced 2×4 grouping halves it; `casterLodBias` keeps
    caster poly counts down. **If it shows up in a profile, that is the signal to reach for indirect-per-
    view** (Phase 4 cull already emits per-cascade lists; keep it in reserve — see decision 12).

11. **"Multi-view culling" (our sense) ≠ hardware multiview.** The Phase-4 cull is **one compute pass
    against N frusta at once** (main camera, each cascade's ortho box, planar reflection, mirrors),
    emitting per-view visibility. It is fully portable (Metal included) and is what makes 8 cascades
    affordable. It is *independent* of the render strategy and feeds **either** multiview-union (decision
    8) **or** indirect-per-view (decision 12). Building it is unconditionally worth it; hardware multiview
    rendering is the desktop overhead optimization layered on top.

12. **Keep indirect-per-view as the portable baseline / reserve.** Where multiview is unavailable or the
    VS-union tax bites (Metal, or profiled hotspots), issue one indirect draw per cascade from the
    per-view cull lists — tight lists, and it *can* do per-cascade LOD. Same cull output, different
    consumer. This is also the natural Metal path until amplification-grouped multiview is worth writing.

13. **Filtering: keep 3×3 HW-PCF near; widen far.** Rotated Poisson-disk PCF with per-pixel jitter on the
    outer cascades buys a soft, wide penumbra that masks coarse texels without an N×N blowup. EVSM/MSM for
    the *far* cascades (Tier 3) is the bigger, Metal-friendly lever — see §4.

14. **Tighten the fit with prepass depth (SDSM-lite) — Tier 2.** The Phase-2 depth prepass
    ([depth-prepass-plan.md](depth-prepass-plan.md)) yields scene min/max depth: pull the last cascade's
    far plane to the furthest *visible* receiver and tighten each cascade's ortho **z-range** (today
    ±radius+zPad ≈ ±900 m for the far slice — bloats bias headroom and forecloses 16-bit). Tightened z
    then unlocks **`Depth16Unorm` for far cascades** (2 B/texel, halves their memory again).

15. **Contact shadows move near detail off CSM.** The prepass plan already notes screen-space contact
    shadows let **CSM be tuned for the medium range instead of millimetre detail**
    ([depth-prepass-plan.md](depth-prepass-plan.md) §intro). That is a *reason* the range push is
    affordable: cascade 0 need not chase millimetre contact, so its extent can grow.

---

## 2. Tiered rollout

### Tier 1 — range + distribution + bias (independent; land now)
No dependency on prepass or GPU cull. Turns "50–100 m, mush after cascade 0" into "several hundred metres,
evenly sharp" purely via retune + contained code:
1. Decouple shadow distance from the 250 m clamp (decision 1).
2. `MAX_CASCADES` 4 → 8 at 2048² + widen `ShadowBlock`/`CascadeSet`/shader loops (decision 2). **Update
   the froxel fog consumer** ([lib.rs:591](../rust/src/lib.rs#L591)) in the same change.
3. Retune `splitCoef` (~0.7–0.8) + `distanceCoef`; expose in the dev panel (decision 3).
4. Closed-vs-single-sided caster split with front-face culling for closed solids (decision 4).

**Exit check:** even texel density across the whole range at 128 MiB; no peter-panning on closed props;
froxel fog unchanged in look.

### Tier 2 — multiview rendering + tight fit (with Phase 2/4)
5. Multiview render path: one parameterized 1..N-views-per-pass path, backend cap table, `base_array_layer`
   grouping, union-list + cascade-bitmask VS collapse (decisions 5–10). Default ON; Vulkan 1 pass / DX12 2.
6. Wire cascade ortho boxes as views into the **Phase-4 multi-view cull** (decision 11); emit union list +
   per-instance cascade mask.
7. SDSM-lite z/far tightening off the **Phase-2 prepass** (decision 14) → `Depth16Unorm` far cascades.

### Tier 3 — filtering + far-range look (bigger bets, after Tier 1)
8. Rotated-Poisson wide PCF on outer cascades (decision 13).
9. EVSM or Moment Shadow Maps for far cascades — filterable, cheap wide soft edges that hide coarseness,
   Metal-friendly; scope to *far* only (EVSM light-leak worst on high depth-complexity, least visible far).
   Cost: colour target + blur per far cascade.
10. Decouple distant *static* shadows into a cached/streamed shadow atlas so far cascades pay only for
    dynamic casters (the real long-term "km of static shadows is cheap" move).
11. PCSS contact-hardening — nice-to-have, noisy/expensive, lowest priority.

---

## 3. Roadmap edges

- **Phase 2 (depth prepass)** — enables SDSM-lite tight fit (decision 14) and contact shadows that let CSM
  drop millimetre near-detail (decision 15). Tier 1 does **not** need it.
- **Phase 4 (GPU-driven multi-view cull + indirect)** — the cascade ortho boxes become cull views
  (decision 11); makes 8 cascades affordable and provides both the union list (multiview) and per-view
  lists (indirect reserve, decision 12). The roadmap already lists shadow cascades as a first-class
  consumer of the arbitrary-camera cull path ([implementation-roadmap.md](implementation-roadmap.md) §2
  Phase 4, principle 2).
- **Froxel fog** — shares `ShadowBlock`; every cascade-count/layout change updates
  [lib.rs:591](../rust/src/lib.rs#L591) in lockstep (decision 2).
- **Terrain raymarched horizon shadows** — unchanged; own the far terrain-self-shadow, combined by `max()`.
  CSM owns objects + near terrain.

---

## 4. Load-bearing gotchas

1. **Range without distribution is worthless.** Lifting the 250 m clamp alone just smears the far cascade
   to 1 km. Ship decisions 1–3 together.
2. **More cascades want *lower* resolution.** The 512 MiB scare is entirely the false assumption that every
   cascade stays 4K. 8×2048² (128 MiB) is the target; density comes from small extent, not high res.
3. **DX12 multiview is a hard 4.** `D3D12_MAX_VIEW_INSTANCE_COUNT` is a fixed `#define`, not per-vendor.
   8 cascades is unavoidably 2 passes on DX12 — plan the grouped path from the start, don't assume 1 pass.
4. **wgpu exposes no multiview limit** — hardcode the cap table and clamp before pipeline creation, or DX12
   fails opaquely at pipeline build.
5. **`multiview_mask` is per-pass, not per-draw.** You cannot give multiview tight per-cascade lists; it is
   union + VS-collapse (decision 8). If that vertex tax hurts, that is when indirect-per-view earns its keep.
6. **Forward-Z is correct for ortho shadows** — do not "upgrade" the shadow map to reversed-Z; ortho depth
   is linear and gains nothing (unlike the main reversed-Z camera).
7. **The sphere fit's stability is deliberate** — `QuantizeShadowRadius` + double-precision world snap in
   `CascadeLightVPStable` is what stops edge-crawl in a heli. Any tighter fit must preserve texel-grid
   snapping or swim returns. This is *not* on the Tier-1 path; leave it unless texel waste is measured.
8. **Metal amplification is ~2 views** — the 1..N-per-pass abstraction (decision 5) is what lets Metal fall
   to 4 grouped passes without a separate renderer. Don't bank the design on 8-in-one-pass anywhere but
   Vulkan.
