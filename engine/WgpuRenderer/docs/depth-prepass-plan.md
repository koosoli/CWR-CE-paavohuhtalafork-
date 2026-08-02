# Plan: Opaque depth + normal prepass (early-Z, sampleable depth, partial G-buffer)

**Repo:** `paavohuhtala/CWR-CE`, branch `new-renderer-infrastructure`
**Renderer:** `engine/WgpuRenderer` (wgpu-native, Rust) + C++ bridge (`EngineWgpu`)
**Status:** Stage 1 IMPLEMENTED in the working tree (2026-07-08, uncommitted; pending user
build + in-game validation). Stages 0/2/3 still planned. Concrete.

> **RND-030 reconciliation (2026-08-02):** the "uncommitted" caveat above is obsolete. Stage 1 was committed on 2026-07-09 and is present in `gfx3d/cull.rs`, `cull.wgsl` and `hiz.rs`.
>
> The status line is left as written rather than rewritten, so the document's own history stays readable. See [RND-030-renderer-plan-reconciliation-20260802.md](../../../docs/roadmap/decisions/RND-030-renderer-plan-reconciliation-20260802.md).

> **Stage 1 bring-up notes.** Shipped: shared `shaders/gbuffer.wgsl` (octahedral view-space
> normal encode/decode); `fs_prepass` (shader3d) + `fs_terrain_prepass` (terrain) reusing the
> colour VS + override constants unchanged; an `Rg16Float` normal G-buffer allocated with the
> depth target (`Gfx3d::normal_view`); prepass pipelines (`PrepassKey{skinned, alpha_ref}`) and
> colour write-off variants (`PipelineKey::depth_write_off`); `Pass3dMode` on `draw_one`
> (Prepass self-filters to the opaque set); terrain `TerrainPass{Color, ColorNoWrite, Prepass}`;
> and the first-segment prepass render pass in `render_frame` (clears depth 0.0 + stencil 0, then
> the colour sub-pass LOADs both and draws the opaque set GreaterEqual/write-off). Skinned draws
> re-skin in the prepass VS (compute skin-bake is the follow-up amortization, before CSM-8 /
> planar reflections). Foliage uses hard alpha discard (A2C deferred to when MSAA lands).
> `WGR_PREPASS=0` is a **temporary** dev A/B toggle for bring-up, not a shipped flag (decision 8).
> Not yet done: Stage 0 instrumentation (GPU timer / overdraw heatmap) and Stage 2 (expose the
> sampleable depth + normal to consumers).
**Roadmap slot:** Phase 2 — the keystone. See [implementation-roadmap.md](implementation-roadmap.md).

> The single most load-bearing piece of renderer infrastructure. It ships **unconditionally on the wgpu
> backend — no flag** (decision 8): it is a hard prerequisite for the intended feature set, not an
> optimization to be toggled. It also has **standalone value today** — an opaque depth prepass gives
> every expensive fragment shader (per-pixel object lighting, the terrain multi-layer blend) early-Z
> rejection of occluded pixels — and it is the shared keystone the intended features consume:
> - **Transparent/refractive water** ([water-rendering-plan.md](water-rendering-plan.md)) — sampleable
>   opaque depth for depth-based colour, soft shorelines, refraction clamping (opaque-only = the seabed
>   depth it wants).
> - **Forward+ clustered lighting** ([forward-plus-plan.md](forward-plus-plan.md)) — overdraw reduction
>   (so the per-cluster light loop runs once per visible pixel) + optional active-cluster culling.
> - **Screen-space AO + shadowing** (future, not scoped here, but the reason the pass writes normals now):
>   the pass lays down a **view-space normal** target alongside depth from the start (a partial G-buffer,
>   decision 9), so **SSAO/GTAO**, **screen-space contact shadows**, and SSR have their inputs ready and
>   the pass never needs reworking. With foliage now in the G-buffer (decisions 3, 10), dense vegetation
>   gets real self-occlusion and contact darkening. Contact shadows in particular let **CSM be tuned for
>   the medium range instead of millimetre detail** — the near, fine-grained occlusion moves to the
>   screen-space term.
> - **GPU occlusion culling** ([gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)) — the
>   Hi-Z pyramid is a reduction of this prepass depth.
>
> This plan **owns** the prepass; those consume it. One prepass, one source of truth.

---

## 0. Where we are today (verified 2026-07-08)

- **No depth prepass anywhere in the crate.** Opaque geometry shades as it rasterizes; occluded
  fragments still run the full fragment shader (overdraw). The one subsystem that already batches well
  (terrain) is also the most expensive fragment shader (4-cell ground blend + detail + per-pixel normal
  + CSM + long-range shadow mask), so it pays the most for overdraw.
- **Depth target:** `DEPTH_FORMAT = Depth24PlusStencil8` ([gfx3d/mod.rs:15](../rust/src/gfx3d/mod.rs#L15)),
  created in `ensure_depth` with **`usage: RENDER_ATTACHMENT` only** ([gfx3d/mod.rs:1406](../rust/src/gfx3d/mod.rs#L1406))
  — **not sampleable**; `depth_view()` lends it as an attachment only ([gfx3d/mod.rs:1414](../rust/src/gfx3d/mod.rs#L1414)).
  No MSAA (`MultisampleState::default()`, sample_count 1, [gfx3d/mod.rs:1291](../rust/src/gfx3d/mod.rs#L1291)).
- **Reversed-Z.** Near = larger depth; depth cleared to `0.0` (far), compare `GreaterEqual`
  ([gfx3d/mod.rs:1281-1287](../rust/src/gfx3d/mod.rs#L1281)); the VS applies `reverse_z`
  ([shader3d.wgsl:92](../rust/src/gfx3d/shader3d.wgsl#L92), [frame.wgsl:149](../rust/src/shaders/frame.wgsl#L149)).
- **Stencil** is cleared to 0 each segment and used **only** by the shadow-darkening pass (EQUAL 0 /
  INCR, [gfx3d/mod.rs:1243-1258](../rust/src/gfx3d/mod.rs#L1243)); all other pipelines leave it default.
- **Multi-segment depth structure.** `plan_3d` ([gfx3d/mod.rs:2118](../rust/src/gfx3d/mod.rs#L2118))
  turns the command stream into ops; segments split at **`ClearDepth`** and **`Resolve`**
  ([lib.rs:707-810](../rust/src/lib.rs#L707)). `ClearDepth` is emitted by `EngineWgpu::Clear(clearZ,…)`
  ([EngineWgpu.cpp:791-804](../EngineWgpu.cpp#L791)) — the classic OFP "clear Z before the first-person
  weapon / near scene". Each segment's 3D subpass **clears depth to 0.0 + stencil to 0**
  ([lib.rs:749,753](../rust/src/lib.rs#L749)); the interleaved 2D subpass loads
  ([lib.rs:768-787](../rust/src/lib.rs#L768)).
- **What's opaque and depth-writing.** `plan_3d` marks a draw *instanceable* iff `blend == Opaque &&
  offset == None && depth == TestWrite && !skinned` ([gfx3d/mod.rs:2167-2170](../rust/src/gfx3d/mod.rs#L2167));
  those bucket into instanced draws. Barriers (transparent, decal/ZBias, skinned, non-standard depth)
  emit in place. **Cutout foliage** is opaque-but-alpha-tested: it writes depth (`TestWrite`) but its
  fragment shader discards by the `alpha_ref` override ([shader3d.wgsl:199-201](../rust/src/gfx3d/shader3d.wgsl#L199)).
  **Terrain** draws through its own pipeline (depth write on, `GreaterEqual`, `cull_mode: None` for
  skirts, [terrain/mod.rs:599-611](../rust/src/terrain/mod.rs#L599)) and flushes the run.

**Consequence:** the prepass's depth-writing set for the main segment is *all opaque objects (pure-opaque
instanced + skinned-opaque + **cutout foliage**) + terrain* — foliage is **in** the prepass, matched to
the colour pass's cutout technique (decisions 3 & 10). Transparents, decals, and the near/weapon segment
are out.

---

## 1. Design decisions

1. **Opaque depth + normal prepass over the main (first) segment, reusing the existing depth target.** A
   new render pass writing **depth plus a view-space normal target** (decision 9), depth-write on,
   clearing depth to 0.0, that draws the opaque set (objects + terrain) *before* the colour pass of the
   same segment. The colour pass then **loads** that depth and shades with early-Z; screen-space effects
   (SSAO, later SSR/contact shadows) read the depth+normal G-buffer. This is the whole feature; the rest
   is consequences.

2. **The colour pass switches opaque draws to `GreaterEqual` + depth-write OFF.** With depth already
   complete, the colour pass needs no depth write; early-Z rejects any fragment not at the front.
   Choosing **`GreaterEqual` (keep the existing compare) + `depth_write_enabled = false`** — *not* the
   textbook `Equal` — is deliberate: it is robust to any sub-ULP difference between prepass and colour
   depth (a colour fragment computing depth a hair *nearer* than the prepass still passes; it can never
   actually be nearer since the prepass already holds the frontmost). `Equal` would crack on the
   slightest VS precision drift. Because the prepass and colour pass share the **same VS + override
   constants**, depth is effectively identical anyway; this just removes the failure mode. Only the set
   that was **in the prepass** flips to write-off — see decision 4.

3. **Prepass pipeline variants — all opaque (incl. cutout foliage) + terrain.** Each writes depth + the
   view-space normal (decision 9) via a **minimal fragment** in one of two flavours:
   - **`alpha_ref == 0`** (pure opaque + terrain): output only the view-space normal — the
     interpolated/skinned object normal transformed to view space, and for terrain the same heightmap
     central-difference normal `fs_terrain` derives ([terrain.wgsl:77-83](../rust/src/terrain/terrain.wgsl#L77)).
     (It would be `fragment: None` if not for the normal target; the fragment stays trivial.)
   - **`alpha_ref > 0`** (cutout foliage): additionally sample the albedo alpha and apply the **same
     cutout technique the colour pass uses** — hard `alpha_ref` discard (MSAA off) or alpha-to-coverage
     (MSAA on, alpha carried in the normal target's alpha channel), so coverage is identical between
     passes by construction (decision 10). Foliage thus lands in the depth **and** normal G-buffer.
   This is the change from the earlier draft: foliage was excluded when the prepass was depth-only (no
   colour target to carry A2C alpha). The normal target removes that obstacle, so **foliage is now in** —
   which is what gives dense vegetation working SSAO/GTAO (canopy self-occlusion, foliage↔ground contact)
   and screen-space contact shadows. All prepass pipelines reuse the **same VS entry, vertex buffers,
   bind groups, and override constants** as their colour counterpart, so depth is bit-for-bit consistent.
   A small fixed number of new pipelines.

4. **The whole prepassed set uses write-off; everything else keeps its current depth-write.** The colour
   pass must know which draws contributed depth — that's the entire opaque set (instanced opaque +
   skinned-opaque + **cutout foliage** + terrain), which all draw `GreaterEqual`/depth-write-off. Foliage
   is included: its covered samples wrote depth in the prepass (via the matched cutout technique,
   decision 10), so per-sample `GreaterEqual`/write-off resolves overlapping foliage correctly (nearer
   foliage's samples win; farther ones fail the test). Transparents, decals/ZBias, and the shadow pass
   keep their current depth state verbatim. All derivable from the existing `PipelineKey` (`blend`,
   `offset`, `depth`, `alpha_ref`) — no new per-draw metadata. Because foliage is now prepassed too,
   foliage-behind-foliage overdraw is also cut, not just foliage-behind-opaque.

5. **Main segment only, initially.** The prepass covers the first (world) depth segment — the big,
   overdraw-heavy one. The near/weapon segment (after a `ClearDepth`) is small; leave it on the current
   single-pass path. Generalizing the prepass per-segment is a later, measured step (§3, Stage 3).

6. **Make the depth target sampleable and expose it.** Add `TEXTURE_BINDING` to `ensure_depth` and a
   depth-aspect view. This *supersedes* the throwaway sampleable-depth path the water plan would
   otherwise self-provide, and is what Hi-Z reduces. Expose via a getter now; promote a
   scene-depth binding into the shared frame group(0) when a consumer needs to sample by screen UV.

7. **Stencil untouched by the prepass.** The prepass writes depth + the normal target, never stencil
   (stencil default/disabled). The shadow-darkening pass still clears stencil per segment and
   depth-tests-no-write against the (now pre-filled) depth — unchanged behaviour, arguably better (depth
   is already complete when it runs).

8. **Unconditional on wgpu — no flag. GL33 untouched.** The prepass is a hard prerequisite for water
   transparency/refraction, light clustering, and SSAO, so it ships always-on rather than behind a toggle
   we'd then have to support both sides of. The cost objection doesn't apply here (§2): this is a
   25-year-old game — low-poly meshes, simple shaders, tiny textures — so the extra vertex pass is
   negligible, and the freed fragment budget is exactly what pays for the "luxuries" (planar reflections,
   high-quality shadow maps, long draw distance, SSAO). A **temporary** dev A/B (git revert / a build-time
   const) is fine while validating correctness during bring-up, but no shipped runtime flag. GL33 keeps
   its current single-pass path.

9. **Write a view-space normal G-buffer from the start (partial G-buffer, not deferred shading).** The
   prepass' one colour attachment is a **view-space** normal target — view space because that is what
   SSAO/GTAO and SSR consume directly, valid without a per-effect world→view transform. Default format
   **`Rg16Float`, octahedral-encoded** (compact, banding-free — precision matters for AO); `Rgba8Snorm`
   (xyz) is the cheaper fallback if memory ever bites (it won't, on this content). This is a **partial**
   G-buffer for screen-space effects only — **shading stays forward** in the colour pass (objects/terrain
   compute their own shading normal as today; they do *not* read this target). That forward-shading choice
   is exactly what keeps MSAA cheap (§5); the normal G-buffer does not move us toward deferred. It is
   written even before its first consumer (SSAO) exists, so the pass never needs reworking to add it
   later. (Optional future micro-opt: terrain's colour pass could read this normal instead of re-taking
   its heightmap central-difference taps — minor, deferred.)

10. **Cutout foliage: one technique, applied identically in prepass and colour — MSAA-off = hard alpha
    discard, MSAA-on = alpha-to-coverage, both with derivative alpha rescaling.** The colour pass and the
    prepass compute the cutout from the **same** albedo alpha and the **same** VS, so their coverage
    matches per sample by construction — which is exactly what lets foliage live in the prepass
    (decisions 3–4) and get depth+normals for SSAO. Details:
    - **MSAA off:** hard `alpha < alpha_ref` discard (today's path, [shader3d.wgsl:199-201](../rust/src/gfx3d/shader3d.wgsl#L199)).
      Binary, identical in both passes.
    - **MSAA on:** **alpha-to-coverage** (`alpha_to_coverage_enabled`), alpha carried in the normal
      target's alpha channel so the prepass has a value to derive coverage from — no longer the
      "no-colour-target A2C" obstacle that forced foliage out of a depth-only prepass.
    - **Derivative / mip-based alpha rescaling** (Castaño's "Computing Alpha Coverage" / Golus's
      "Anti-aliased Alpha Test"): rescale alpha by `fwidth`/mip so average coverage is preserved with
      distance — fixes foliage **thinning out and vanishing at range** and sharpens edges. Deterministic,
      so prepass/colour coverage stays matched. Applied in both passes.
    - **Rejected here:** *hashed alpha testing* (Wyman-McGuire) — smoother/stochastic but wants TAA to
      denoise, which this engine lacks; and *OIT* — genuinely smooth but breaks the single-depth /
      prepass / SSAO model. A2C + alpha rescale is the sweet spot: MSAA-native, prepass-compatible, no
      asset changes. Honest limit: coverage is quantized to the sample count, so edges are "stable and
      good," not glass-smooth (TAA would be the next step, out of scope).

---

## 2. Cost model (why the "is it worth it?" question doesn't arise here)

> **Measured 2026-07-08 (after bindless textures landed), user hardware.** Simple scene: 1.8ms without /
> 1.9ms with (prepass 0.20ms) — neutral, no overdraw to reject, as predicted. Complex scene (mountain top,
> vegetation, water, early-morning sun + fog + CSM): **3.9ms without → 2.3ms with** (prepass 0.25ms) — a
> **1.6ms / ~41% frame-time cut**, i.e. ~1.85ms of gross fragment work rejected. This is *before* Forward+
> / SSAO / water-refraction add per-fragment cost, each of which compounds the win. Confirms the model
> below: neutral when fragment-cheap, large when fragment-bound + overdrawn.

- **Win:** each occluded opaque pixel skips its fragment shader. The heaviest shaders in the scene
  (terrain 4-cell blend + CSM + mask; per-pixel object lighting; the ≤256-light loop) are exactly the
  ones overdraw multiplies. Forward+, water, and SSAO raise fragment cost further, compounding the win.
- **Cost is negligible on this content.** The extra pass is vertex-only over opaque geometry, and the
  game is 25 years old: low-poly meshes, simple shaders, tiny textures. The classic prepass objection —
  "vertex-bound scenes with cheap fragments lose" — does not apply; we are nowhere near vertex-bound, and
  the Phase-4 GPU-driven path makes the prepass nearly free to *submit* (it replays the same indirect
  draw args). This is why it's unconditional (decision 8), not a measured trade-off.
- **It's a prerequisite regardless.** Even if it were a wash on frame time, water transparency/refraction,
  light clustering, and SSAO all *require* opaque scene depth. There is no version of the target feature
  set that skips it.
- **Normals from the start (partial G-buffer).** The pass writes a view-space normal target alongside
  depth (decision 9), so SSAO/SSR/contact-shadows have their input ready and we never rework the pass to
  add it. The added cost is a trivial fragment (output one normal) — negligible on this content. It stays
  **forward** and MSAA-compatible (§5): this is a *partial* G-buffer for SS effects, **not** a move to
  deferred shading (lighting stays in the forward colour pass, which is what keeps MSAA cheap).
- **Complementary foliage AO stays asset-free.** SSAO/GTAO on the G-buffer is a *contact* term; a bush's
  volumetric self-occlusion also wants a coarse ambient term. Any such addition must be **computed at
  load, never hand-authored** — the project constraint is "any technique that doesn't require
  hand-editing the existing 25-year-old assets." So: per-vertex AO computed procedurally from the mesh at
  load, or a procedural bottom-of-canopy darkening from local model-Y — not artist-painted vertex AO or
  re-authored alpha maps. Out of scope for this plan; noted so the option is understood to be open.
- **Tooling, not go/no-go:** Stage 0's GPU timer + overdraw visualization quantify the win and serve the
  broader perf work — they are not gating a keep/drop decision.

---

## 3. Stages

### Stage 0 — Instrumentation
- RenderDoc debug group around the new prepass slot; GPU timestamp query around the main colour segment;
  an overdraw heatmap debug mode (increment on each opaque fragment). Capture before/after overdraw +
  frame-time on a heavy scene (dense town, forested coastline) — to *quantify* the win and to seed the
  broader perf work, not to decide whether to keep the prepass (decision 8).
- **Exit:** overdraw/timing visibility in place.

### Stage 1 — Depth + normal prepass + early-Z colour pass (the standalone win)
- Add prepass pipeline variants (all opaque incl. cutout foliage + terrain) with the minimal
  view-space-normal fragment (decision 3) and the `Rg16Float` octahedral normal target (decision 9).
  Foliage's fragment applies the matched cutout technique (hard discard now; A2C when MSAA lands —
  decision 10).
- In `render_frame`, for the **first segment**: emit the prepass (normal colour attachment + depth,
  clear depth 0.0) replaying the segment's full opaque set (instanced opaque + skinned-opaque + cutout
  foliage + terrain), then run the existing colour subpass with depth **Load** and the prepassed
  pipelines switched to `GreaterEqual`/write-off (transparents/decals unchanged). Always on (decision 8).
- Reuse `plan_3d`'s opaque classification to build the prepass draw list — no new submission from C++.
- **Exit:** output pixel-identical to the pre-prepass path (verified with a temporary dev A/B, then the
  old path is removed), overdraw/frame-time win quantified, and the depth+normal G-buffer populated
  **including foliage** (ready for SSAO even though no consumer exists yet). Ships on its own.

### Stage 2 — Expose sampleable depth + normal (unblocks consumers)
- Add `TEXTURE_BINDING` + depth-aspect view to `ensure_depth`; expose depth **and** the normal target via
  getters and, when first needed, a group(0) scene-depth (+ normal) binding (world-xz/screen-UV
  reconstruction helper in `frame.wgsl`).
- **Exit:** water ([water-rendering-plan.md](water-rendering-plan.md)) and Hi-Z
  ([gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)) consume the prepass depth (water drops
  its self-provided fallback); the normal target is available for SSAO when it lands.

### Stage 3 — Coverage + GPU-driven (later, measured)
- Extend the prepass to additional depth segments (near/weapon) if profiling wants it. Fold the prepass
  replay into the Phase-4 indirect path so it costs one more `multi_draw_indirect`, not a CPU re-walk.
  When MSAA lands, switch foliage's cutout from hard discard to alpha-to-coverage in both passes
  (decision 10) — one flag, coverage stays matched.
- **Exit:** prepass is GPU-driven and covers whatever segments profiling justifies.

---

## 4. Load-bearing hazards

- **`Equal` vs `GreaterEqual` on the colour pass.** Use `GreaterEqual` + write-off (decision 2). `Equal`
  is a precision trap on float reversed-Z even with an identical VS.
- **Reversed-Z clear.** Prepass clears depth to **0.0** (far) and writes `GreaterEqual`, same as today.
  A conventional-depth clear/compare here silently rejects everything or nothing.
- **VS parity.** Prepass and colour VS (and their override constants: `depth_bias`, skinning, conform)
  must match, or write-off early-Z drops/keeps the wrong fragments. Reuse the same VS entry + constants;
  do **not** give the prepass a cheaper/approximate transform.
- **Which draws flip to write-off.** Exactly the prepassed opaque set. Getting a *transparent* or a
  *decal* onto write-off would break its intended over-draw. Derive strictly from the pipeline key.
- **Multi-segment correctness.** Only the prepassed segment's colour pass may Load depth; segments after
  a `ClearDepth` still clear. Don't Load a stale depth across a `ClearDepth` boundary.
- **MSAA.** A first-class *future* requirement, not an afterthought — see §5. The prepass is designed so
  MSAA is a `sample_count` parameter, but the sampleable-depth path needs an explicit resolve.

---

## 5. MSAA readiness (design constraint, not a later rewrite)

MSAA is a **primary, intended** capability — it is the main reason this renderer stays **forward** rather
than deferred (deferred can't MSAA cheaply), and it is required for **alpha-to-coverage** foliage. The
prepass must not paint us into a no-MSAA corner. Nothing here is built yet, but every decision above is
made so turning MSAA on is a parameter change plus one resolve pass, not a redesign:

- **Sample count is a parameter.** The prepass, colour, and (Forward+) shading pipelines all take the
  scene `sample_count`. The prepass depth **and normal** targets are allocated multisampled at the same
  count as the colour target; early-Z write-off works per-sample unchanged. Today
  `MultisampleState::default()` (count 1) everywhere — MSAA flips one number.
- **The normal target resolves like the depth.** Under MSAA the view-space normal G-buffer is
  multisampled; SSAO wants a single-sample normal, so it resolves alongside the depth (average or
  sample-0 — normals are lower-frequency than depth; a colour `resolve_target` works directly for the
  normal, no manual pass, unlike depth). Without MSAA both are single-sample and the resolves are no-ops.
- **Sampleable depth needs a resolve pass (WebGPU has no depth-resolve attachment).** A multisampled
  depth can only be read per-sample (`texture_depth_multisampled_2d` + `textureLoad(sample)`). So under
  MSAA, add a tiny **depth-resolve** pass reducing the MS depth to a single-sample `R32Float`. Use a
  **`min`** reduction (reversed-Z: farther = smaller = conservative) — the *same* conservative depth the
  Hi-Z pyramid wants ([gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md)), and low-frequency
  enough for water's depth-based colour. So Stage 2's "expose sampleable depth" becomes "expose the
  resolved single-sample depth"; consumers (water, Hi-Z) sample that and never know MSAA is on. Without
  MSAA the depth is already single-sample and the resolve is a no-op/skip.
- **Alpha-to-coverage runs in *both* the prepass and the colour pass, identically (decision 10).** Under
  MSAA, cutout foliage enables `alpha_to_coverage_enabled` and outputs alpha (carried in the normal
  target's alpha channel in the prepass); because both passes derive the coverage mask from the same
  alpha and same VS, they agree per sample — so foliage's depth+normal write in the prepass matches its
  colour-pass coverage, and early-Z write-off is consistent. This is what lets foliage be **in** the
  prepass (it's out only when the prepass is depth-only, with no colour target to carry the A2C alpha).
  Pair with derivative alpha rescaling (decision 10) for distance-stable edges.
- **Forward+ shading is per-pixel under MSAA** (one shade per pixel; MSAA gives edge AA); the cluster
  lookup uses the pixel's depth. Genuinely deferred lighting would force per-sample G-buffer handling —
  another reason the roadmap stays forward. See [forward-plus-plan.md](forward-plus-plan.md).
- **The HDR resolve chain gains one step.** When MSAA lands, the `Rgba16Float` scene target is
  multisampled and must resolve to single-sample HDR before the tonemap/bloom/exposure chain (a colour
  `resolve_target`, which WebGPU *does* support). Out of scope for the prepass itself, flagged so the
  renderer-wide MSAA switch accounts for it.

---

## 6. Cross-references
- [implementation-roadmap.md](implementation-roadmap.md) — Phase 2 keystone; unblocks Phases 3–5.
- [forward-plus-plan.md](forward-plus-plan.md) — consumes the prepass for overdraw reduction / active-cluster culling (cluster grid itself is prepass-independent).
- [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) — reduces this prepass depth into the Hi-Z pyramid (reversed-Z → `min`).
- [water-rendering-plan.md](water-rendering-plan.md) — samples the prepass depth (Phase 3).
- [rendering-performance-plan.md](rendering-performance-plan.md) — the prepass complements its Stage 1–3 batching/instancing (overdraw vs API-call overhead are orthogonal wins).
