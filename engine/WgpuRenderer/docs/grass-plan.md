# Grass — what ships, and the gate that does not

This exists because the grass system's design record lived entirely in `/.agents/`,
which is **gitignored**: no clone has it. A shipped renderer system with no tracked
plan is the RND-030 failure exactly — nothing to assign work against, and nothing a
reader can check. This is the tracked replacement, written against the branch.

## 1. What ships

Three GPU-generated LOD rings, all placed by compute, none by the CPU:

| ring | grid | geometry | measured (reference mission) |
|---|---|---|---|
| near | 512² candidates | real blade geometry — two crossed ribbons, five segments, wind-bent per blade | 48 647 instances, 2 918 820 verts |
| mid | 384² candidates | crossed cards keeping a real blade silhouette | 14 165 instances |
| far | 384² candidates | one terrain-conforming coverage quad per compacted cell | **off by default** (`farRadius` gates it) |

The mid ring is deliberately not a shortcut between near and far: a two-level field
puts an obvious quality cliff at 25–50 m, and the third level is what removes it.

Only the near ring casts shadows. That is a cost decision, not an oversight.

**Measured GPU cost** (original training mission, 3441×1440, Vulkan, RTX 3070 —
`docs/roadmap/evidence/preview0-wgpu-rel000-mission-20260805-original-training-capture.json`):

```
Grass prepass  0.970 ms
Grass colour   1.053 ms
Grass shadow   0.156 ms
               ------
               2.179 ms of a 24.178 ms frame  (9.0%)
```

## 2. Surface detail: procedural by default, authored when present

Two independent texture paths, each with a silent, deliberate fallback.

**Near blades** — `blade_atlas.rs` generates an 8-layer `64×256` `Rgba8UnormSrgb`
array on the CPU at init. A texture **array**, not a column atlas: an atlas averages
neighbouring columns in the lower mips, so a distant blade would bleed into the
flower beside it. Layers mip independently.

It is generated rather than shipped for a licensing reason worth keeping visible:
this repo is GPL, and the game's own grass textures are APL-SA. Species ordering
(`0..4` grass, `4..6` weed, `6..8` flower) is an ABI shared with `grass.wgsl`, which
cannot import Rust — tests are what keep the two definitions honest.

The blades are **opaque**. The silhouette is geometry, so there is no alpha cutout
and no discard, and the near colour pass keeps early-Z.

**Mid clump** — procedural crossed ribbons, overridden by a clump alpha when found.

Both are replaced by authored images if they exist on disk, via `UploadGrassTuft()`
and `UploadGrassBladeAtlas()` in `TerrainWgpu.cpp`. Absence logs at INFO and falls
back. Corrupt or mismatched images fall back the same way; neither can prevent
startup.

Paths are relative to the **working directory**, which is the game install, not the
repo — so deploying is a three-part copy: exe, DLL, and the `assets/` tree.

### Why the 2001 texture is not the fallback

The game ships `data/trava1_pmp2.pac`, whose opaque texels average
`(0.322, 0.375, 0.334)` — blue level with red, so grey-teal rather than green.
Falling back to it would give anyone without the optional asset a **worse** picture
than no photo cards at all. It is reachable only by pointing `WGR_GRASS_TUFT` at a
converted copy.

## 3. GRS-GATE-1 is not passed, and the reason is provenance

The rendering half of the gate is done: near/mid/far exist, the procedural fallback
works and is deliberate, original game data is untouched, and missing textures
cannot prevent startup.

What is missing is everything about where an authored texture comes from:

- **`assets/` is gitignored.** The handoff notes call it "source of truth (committed
  to the repo)"; it is not committed and never was. So on a fresh clone the authored
  path is dead and every user gets the procedural look. Build outputs are therefore
  not reproducible in the sense the gate means.
- **No source registry (`ASSET-010`).** No source URL, provider, author, licence,
  licence URL, download date, or archive hash exists for any current image.
- **No deterministic processing recipe.** The blade layers were produced by hand;
  nothing can rebuild them from a source.
- **No validated optional package**, so nothing checks that a shipped set is
  complete, correctly sized, or the one the registry describes.

The project's own grass plan already required this — "no copied texture asset
without separate licence verification" — and it has not happened.

### The order this has to be done in

```
one licence-compatible source, verified on the ORIGINAL provider page
  → registry entry with hashes
  → deterministic processing recipe
  → validated optional package
  → texture array
  → near/mid/far rendering            (already done)
  → procedural fallback when absent   (already done)
  → visual and performance comparison
```

Aggregator licence labels are not authoritative. A search result saying
"public domain" records what the aggregator believes; the registry must cite the
original provider's own statement.

## 4. Downwash and interaction

`WgrGrassParams` carries tracks, an interactor, and a downwash array, so rotor wash,
vehicle tracks and flattening already drive the placement compute. `GRS-040` requires
this behaviour be *preserved* — rotor speed, altitude, inertia, multi-helicopter,
visible deformation, and matching shadow response — before any of it is improved.
Treat the current behaviour as the baseline to beat, and capture it before changing
it (`TEST-GRS-001`).
