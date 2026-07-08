# Plan: compute-shader skinning bake (`skin_bake`)

**Status:** Phase 1 IMPLEMENTED + validated (2026-07-08), **default OFF** (`WGR_SKIN_BAKE`
opt-in). Landed right after the depth prepass + bindless textures. See **§Phase-1 as-built
corrections** below for where the implementation deviates from the original design.

**Perf verdict (measured 2026-07-08, infantry-heavy scene, 4 CSM cascades): standalone,
the bake is pure overhead — do not ship it on until a consumer exists.** With bake:
2.5 ms (0.3 ms bake). Without: 2.2 ms. The shadow (0.22 ms) and prepass (0.15 ms) times
are **identical** with/without the bake — removing VS skinning from those passes saved
nothing, because VS skinning is ~free for OFP's low-poly characters. The original
motivation (amortize skinning across 6→10+ passes) is empirically weak for this content:
"6× free" is still free, and it won't flip at 8 cascades. The bake only adds a VRAM
round-trip + per-slot dispatches. So it is **default off** — kept as correct, validated,
exercisable (`WGR_SKIN_BAKE=1`) infrastructure, NOT a shipped win.

**Where it pays off — and why there is no standalone "Phase 2":** the bake's real value is
that baked skinned = **rigid geometry**, which is the hard prerequisite for GPU-driven
rendering (the GPU generates draws indirectly and cannot run a per-object VS skinning pass).
Phase 2's draw coalescing (`multi_draw_indexed_indirect`) and batched dispatches are a
**subset of** [gpu-culling-and-depth-plan.md](gpu-culling-and-depth-plan.md) (its Stage 6),
so they are folded there rather than built twice — the baked-output layout is co-designed
ONCE against that plan's geometry pool. The one thing NOT to do is finalize the batched
dispatch / output layout in isolation now: it would pin a layout the geometry pool redefines.
Phases 2-3 below are the original design; **read them through Stage 6 of the GPU-driven plan,
which now owns them.**

## Phase-1 as-built corrections

Reading the real code turned up several places the original design (below) was stale or
under-specified. The as-built implementation:

1. **The baked vertex is 36 bytes, not 32.** `WgrMeshVertex` is
   `pos(12) + norm(12) + uv(8) + conform(4)` = **36 B / 9 words** — the 4th field is a
   per-vertex terrain-conform selector read at `@location(5)` by the rigid `vs_main` /
   shadow `vs_solid`/`vs_alpha`. The bake output must match the rigid `vbuf_attrs` stride
   exactly (location 5 lives at byte 32), so the bake writes **9 words** and **passes the
   conform word through verbatim**. (Skinned characters are never terrain-conformed veg, so
   their conform is always 0 and the rigid pipeline's mode-0 branch ignores it — but the
   stride still has to line up.)
2. **The compute shader reads/writes `array<u32>` and `bitcast`s, not `array<f32>`.** This
   makes the uv + conform passthrough **bit-exact** (copying the conform word through an
   `f32` variable would risk the GPU flushing its tiny denormal bit-pattern to zero). Bones
   are unpacked with shifts/masks and weights with `f32(byte)/255.0` — **no `unpack4xU8`
   dependency** (portable across backends; the original plan flagged this builtin as
   uncertain).
3. **Dual-path validation toggle instead of deleting the skinned pipelines.** Mirroring how
   the depth prepass shipped (`WGR_PREPASS`), the VS-skinning path is kept behind
   **`WGR_SKIN_BAKE`** (default **on** = bake path). `WGR_SKIN_BAKE=0` falls back to the
   original per-pass VS skinning for A/B. The palette upload is **gated, not duplicated**:
   when the bake is on, the palette is uploaded once as a **storage** buffer (`palette_buf`)
   for the compute pass and the old dynamic-offset UBO (`self.palette`) + `group1_skinned`
   bind are **skipped**; when off, the reverse. So neither mode pays for the other. `skin.wgsl`
   is **untouched** (still the UBO palette for the fallback VS path) — the bake inlines its
   own copy of the 4-line blend math against the storage palette in `skin_bake.wgsl`.
   Deleting the VS path + `skin.wgsl` palette binding + skinned pipelines is a **follow-up**
   once the bake is validated in-game.
4. **The bake plan spans BOTH draws and shadow casters,** deduped by `palette_slot`
   (1:1 with a mesh+pose, since a palette block is one skeleton's skinning). It is built in
   a new `Gfx3d::prepare_skin_bake(draws, casters, palette)` called **before** both
   `prepare_shadows` and `prepare`, so those two can pack an **identity world** into the
   per-instance world SSBO / `ShadowCasterGpu` for every baked entry (the baked position
   already has the camera-relative world folded in via the palette, so downstream must not
   re-apply it). `skin_base_vertex: FxHashMap<slot, out_base_vertex>` is the draw/caster
   side's lookup.
5. **base_vertex is the only draw-side change.** A baked draw/caster routes through the
   **rigid** color/prepass/shadow pipeline (`skinned = false`), binds `skinned_vbuf` at
   vertex slot 0, and issues `draw_indexed(range, base_vertex = skin_base_vertex[slot], ..)`
   — identical to a rigid draw otherwise. Indices are unchanged (0-based within the mesh).
6. **Phase 1 leaves skinned draws as count-1 barriers in `plan_3d`** (no coalescing) — the
   crowd-instancing (`instance_count > 1`, `multi_draw_indexed_indirect`) is Phase 2, exactly
   as the design intends. `skin_bake(encoder)` is recorded as the **first** thing in the
   frame encoder, before the shadow cascades and the prepass, so wgpu's automatic
   storage→vertex barrier covers every later read.
7. **The group(0) bind is cached by mesh, not rebuilt per dispatch per frame.** A RenderDoc
   capture (dozens of soldiers) showed one `vkUpdateDescriptorSets` per skinned mesh **every
   frame** plus a `write_buffer` copy per `BakeParams` — pure phase-1 overhead with no
   correctness role. Fixed: the group(0) bind `{vbuf, skin, palette_buf, skinned_vbuf}` is
   whole-buffer for palette/output and per-mesh-constant for vbuf/skin, so it is **cached in
   `bake_bind_cache: FxHashMap<MeshKey, BindGroup>`**, rebuilt only when `palette_buf` /
   `skinned_vbuf` regrows (rare) or a mesh is destroyed/reskinned — **zero descriptor updates
   on steady frames**. `BakeParams` for all groups upload in **one** `write_buffer` (a strided
   scratch buffer) instead of one copy each. `skin_bake` skips the group(0) rebind when the
   mesh repeats (group(1) stays a cheap dynamic-offset rebind). Per-instance *dispatches*
   remain (Phase 2's instanced-per-mesh dispatch is the next lever, and only helps when
   soldiers share a mesh handle — the bake time itself, ~0.29 ms for dozens of soldiers, is
   dominated by the compute, not the binds).

The original design (below) still governs the data model, the shared-space invariant, the
instancing-readiness, and Phases 2-3.

---

**Status (original):** deferred — design agreed, not yet started. Other renderer work (Forward+) is ongoing; land this alongside/after the depth-prepass work.

## Motivation

Today skinning is evaluated on-demand in the vertex shader on *every* pass that
touches a skinned mesh. `skin_pos`/`skin_normal` (`rust/src/shaders/skin.wgsl`)
are called from both the lit pass (`gfx3d/shader3d.wgsl`, `vs_skinned`) and the
shadow depth pass (`gfx3d/shadow_depth.wgsl`, `vs_skin_solid`/`vs_skin_alpha`).
So a skinned vertex is already skinned up to **5×/frame** (4 shadow cascades +
forward). Forward+ adds a depth prepass → **6×**, for zero added quality.

Migrating to a **compute pre-skin ("bake")** turns that into **1× compute + 5–6
cheap buffer reads**: skin once into a buffer, every pass reads plain
pos/normal/uv. Break-even is below 2 passes, so at 6 passes this is a clear win.
It also lightens exactly the passes we are multiplying (shadow + prepass VS
become near fixed-function), cuts per-vertex bone/weight fetch bandwidth 6×, and
lets us **delete the skinned pipeline variants** (fewer permutations).

This is an **infantry/crew** optimization (skinned geometry only), not a terrain
one — but OFP/CWA gameplay is infantry-heavy, so the fraction is meaningful.

Motion vectors / TAA are **explicitly out of scope** for now (deferred). If added
later, double-buffer the baked buffer for previous-frame positions.

## The invariant that makes this clean

Both shadow and lit passes already share **one** palette buffer, and the palette
folds in a **camera-relative** `world` (`palette[i] = world * bone[i]`). So
`skin_pos` yields a **camera-relative world-space position every pass agrees on**
— shadow applies `light_vp`, lit applies `proj*view`. Therefore **one bake feeds
all 6 passes**. This holds per-instance because each instance's `world` is folded
into its own palette block.

## Instancing-readiness (decided up front)

Phase 1 does **not** implement instancing, but the *data model* is chosen so
instancing later flips two switches with no rework:

1. **Palette is an instance-indexed storage buffer** (`palette_buf[block*128 + bone]`),
   not a dynamic-offset UBO.
2. **The bake is driven by a per-instance table, grouped by mesh.** One dispatch
   per distinct skinned mesh covers all its instances. Phase 1 = one instance per
   group; a crowd = one group, many instances — *same shader*.
3. **Skinned draws route through the rigid pipelines immediately** (identity
   world), so the palette's only consumer is the compute bake (no dual
   UBO/storage maintenance). A/B validation is against the previous commit / GL33,
   not a simultaneously-live skinned pipeline.

Two very different instancing cases:
- **Rigid instancing** (trees/rocks/buildings): never skins — orthogonal, one
  vbuf + per-instance world-matrix buffer. Untouched by this work.
- **Skinned instancing** (a crowd sharing one mesh, different poses): skinned
  geometry is **inherently per-instance** (each pose deforms differently), so you
  can't share a vertex buffer across instances. The bake **is** the per-instance
  expansion: read 1 rest-pose mesh + N palettes → write N baked copies contiguously
  → draw side becomes a rigid multi-draw problem.

## Naming

`skin_bake` — the compute pass "bakes" skinned geometry into a buffer.
`skin.wgsl` keeps the shared `skin_pos`/`skin_normal` math. (Chosen over
`preskin`, `deform`, `skin_compute`.)

## Design

### Baked output is byte-identical to `WgrMeshVertex`

The bake writes `pos(vec3)+norm(vec3)+uv(vec2)` = 32 B (uv copied through). The
baked buffer is then a **drop-in replacement for `mesh.vbuf`**, consumed by the
existing rigid pipelines and `vbuf_attrs` with **no new vertex layout**. Costs 8
duplicated uv bytes/vertex — worth it for touching zero downstream pipelines.

### Route skinned instances through rigid pipelines + identity world

Baked positions already have `world` baked in, so downstream must **not** re-apply
it. `vs_main`/`vs_solid`/`vs_alpha` do `... * world * pos`; bind `world = identity`
for baked instances and they Just Work. This is what lets us delete the skinned
pipelines rather than keep them live.

### New state on `Gfx3d`

```rust
// One STORAGE|VERTEX buffer; every skin instance's baked verts, base_vertex
// addressed. Layout per vertex = WgrMeshVertex (32B).
skinned_vbuf: Option<wgpu::Buffer>,
skinned_cap: u64,

// Palette as instance-indexed STORAGE buffer: block k = matrices [k*128 .. k*128+128).
palette_buf: Option<wgpu::Buffer>,
palette_cap: u64,

// Per-frame bake plan, grouped by mesh.
struct BakeGroup { mesh: MeshKey, palette_base: u32, out_base_vertex: u32, instance_count: u32, vert_count: u32 }
bake_groups: Vec<BakeGroup>,
// draw/caster palette_slot -> baked base_vertex (draw side finds its slice).
skin_base_vertex: HashMap<u32, u32>,

skin_bake_pipeline: wgpu::ComputePipeline,
skin_bake_layout: wgpu::BindGroupLayout, // {in_vbuf ro, in_skin ro, palette_buf ro, out rw}
```

### `skin_bake.wgsl` (already the instanced shader)

```wgsl
#import skin::{skin_pos, skin_normal}

@group(0) @binding(0) var<storage, read>       in_v:  array<f32>;   // 8 f32/vertex, WgrMeshVertex packed
@group(0) @binding(1) var<storage, read>       in_s:  array<u32>;   // 2 u32/vertex: bones, weights
@group(0) @binding(3) var<storage, read_write> out_v: array<f32>;
// palette in skin module, storage buffer indexed by absolute block.

struct BakeParams { vert_count: u32, instance_count: u32, palette_base: u32, out_base_vertex: u32 };
var<uniform> gp: BakeParams;   // small per-dispatch uniform (or push/immediate)

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let inst = gid.x / gp.vert_count;
    let v    = gid.x % gp.vert_count;
    if (inst >= gp.instance_count) { return; }

    let i = v * 8u;
    let pos  = vec3<f32>(in_v[i],    in_v[i+1u], in_v[i+2u]);
    let norm = vec3<f32>(in_v[i+3u], in_v[i+4u], in_v[i+5u]);
    let bones   = unpack4xU8(in_s[v*2u]);
    let weights = unpack4x8unorm(in_s[v*2u + 1u]);

    let block = gp.palette_base + inst;
    let sp = skin_pos(block, pos, bones, weights);    // skin.wgsl now takes block index
    let sn = skin_normal(block, norm, bones, weights);

    let o = (gp.out_base_vertex + inst*gp.vert_count + v) * 8u;
    out_v[o]=sp.x; out_v[o+1u]=sp.y; out_v[o+2u]=sp.z;
    out_v[o+3u]=sn.x; out_v[o+4u]=sn.y; out_v[o+5u]=sn.z;
    out_v[o+6u]=in_v[i+6u]; out_v[o+7u]=in_v[i+7u];   // uv passthrough
}
```

Phase 1: `instance_count == 1` for every dispatch — degenerates but is the crowd
shader unchanged.

### `skin.wgsl` change

Palette `uniform` (128 fixed, dynamic-offset) → `var<storage, read> palette:
array<mat4x4<f32>>` indexed by absolute block; `skin_pos`/`skin_normal` gain a
`block: u32` param (`base = block*128u`). Only the compute bake uses this module
after phase 1.

### Frame flow

1. **`prepare`:** upload palettes into `palette_buf` (flat, one block per
   instance). Walk `draws` + `shadow_casters`, dedup skin instances by
   `palette_slot`, **group by mesh** into `bake_groups`, assign
   `palette_base`/`out_base_vertex`, grow `skinned_vbuf`, fill `skin_base_vertex`.
2. **`skin_bake(encoder)`** before shadow + depth prepass: one compute pass; per
   `BakeGroup` bind mesh vbuf/skin + `palette_buf` + `skinned_vbuf`, set
   `BakeParams`, dispatch `ceil(instance_count*vert_count / 64)`. wgpu inserts the
   storage→vertex barrier automatically.
3. **Shadow / prepass / forward:** baked casters/draws bind
   `skinned_vbuf.slice(base_vertex*32 ..)`, use rigid pipelines + identity world.
   Drop palette bind and skin vertex buffer.

### Draw side

- **Phase 1:** each `WgrDraw3D`/caster is one object →
  `draw_indexed(range, base_vertex = skin_base_vertex[slot], 0..1)`. Only
  `base_vertex` differs from a rigid draw.
- **Instanced (phase 2):** coalesce same-mesh + same-material baked draws (laid
  out contiguously with `base_vertex = j*vert_count`) into one
  `multi_draw_indexed_indirect` or a `0..N` instanced draw with programmable
  vertex pulling. Draw-submission optimization only — **no buffer-model or FFI
  change.**

### FFI

**None in phase 1.** The C++ side already submits palette blocks + `palette_slot`
on both `WgrDraw3D` and `WgrShadowCaster`. Everything here is Rust-internal.
Instancing may later add an optional instance/batch hint to `WgrDraw3D`, but the
renderer can also auto-coalesce — keep the C ABI frozen until that proves
insufficient.

## Phasing

| Phase | Builds | Notes |
|---|---|---|
| **1** | `skin_bake.wgsl`, `palette_buf` (storage), mega-`skinned_vbuf`, `bake_groups` (1 inst/group), reroute skinned→rigid+identity world, delete `vs_skinned`/`vs_skin_*` + skinned layouts/pipelines | data model final; execution 1-per-group |
| **2** | Flatten dispatches per mesh group (N instances); coalesce draws into `multi_draw_indexed_indirect` | flips instance_count>1 + batched draw |
| **3** | Optional FFI instance hint; retire C++ CPU-skin `mesh_update` path | — |

## Sizing & gotchas

- **VRAM:** `skinned_vbuf` = `Σ vert_count` over *all instances* × 32 B (a crowd
  of N sharing a mesh costs `N*vert_count*32`, not one copy). `palette_buf` =
  instances × 128 × 64 B (8 KB/instance). 64-soldier crowd ≈ 512 KB palette + a
  few MB baked verts. Grow-only, `next_power_of_two`.
- **`unpack4xU8`** needs the WGSL packed-integer feature — confirm naga/wgpu 29
  exposes it on target backends; else unpack bones with shifts/masks.
- **Alignment trap:** use flat `array<f32>` for storage access. WGSL `vec3` is
  16-byte aligned, so a `struct { vec3, vec3, vec2 }` is 48 B and won't alias the
  packed 32-B `WgrMeshVertex`. Vertex *fetch* is fine (explicit attribute offsets).
- **Identity-world plumbing** on both rigid `vs_main` and shadow
  `vs_solid`/`vs_alpha`, or the baked-in world double-applies.
- **Add `STORAGE` usage** to `mesh.vbuf` and the skin buffer (currently `VERTEX`-only,
  see `mesh_create` / `mesh_set_skin`).
- **Normals:** the bake reproduces `skin_normal` exactly (palette-matrix multiply,
  no inverse-transpose) — behavior-preserving; do not "fix" here.
- **Variable vert-count within a mesh group can't happen** (instances share the
  mesh) — that keeps `out_base + inst*vert_count` valid for a single dispatch.
- **Empty frames:** guard `bake_groups.is_empty()` so no empty compute pass opens.

## Key source references

- `rust/src/gfx3d/mod.rs` — `Gfx3d`, `prepare`, `draw_one`, `render_shadow_passes`,
  `mesh_create`/`mesh_set_skin`, pipeline/layout construction.
- `rust/src/shaders/skin.wgsl` — shared skinning math + palette binding.
- `rust/src/gfx3d/shader3d.wgsl` — lit pass (`vs_skinned`).
- `rust/src/gfx3d/shadow_depth.wgsl` — shadow cascades (`vs_skin_*`).
- `rust/src/ffi.rs` — `WgrMeshVertex` (32B), `WgrDraw3D`, `WgrShadowCaster`,
  `palette_slot`, `NO_PALETTE`.
