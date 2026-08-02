# Water performance baseline — 2026-08-02

`WTR-GATE-1` requires a performance baseline before ocean production can close.
Both the LOD-0 skew and the roughness double-count were recorded as
unactionable precisely because none existed. This is that baseline.

## Method

Measured through `triWaterGpuMs` / `triWaterNodeCount` / `triWaterStats`, which
read the renderer's own per-frame water statistics. Sampled three times at
120-frame intervals after a 120-frame settle, so the figures are steady-state
rather than first-frame.

| | |
| --- | --- |
| Renderer | WGPU (pinned; water observability is WGPU-only) |
| GPU | NVIDIA GeForce RTX 3070 |
| World | Malden (`abel`) |
| Mission | `tests/integration/missions/water_alive.abel` |
| Camera | Player at the 00training start position, ground level, settled |
| Binary | `dist/x64-win-rel/PoseidonGame.exe` |

The viewpoint is fixed by the mission, so this is repeatable — but it is **one
viewpoint at ground level**. It is a regression anchor, not a characterisation
of water cost across the game.

## Numbers

| Metric | Value |
| --- | --- |
| Water GPU total | **2.95 / 3.30 / 3.81 ms** (three samples), `triWaterStats` reported **3.18 ms** |
| CDLOD nodes selected | **28** |
| Triangles | **516,096** |
| LOD distribution | `lod0=27`, `lod1=1`, `lod2…9=0` |

Spread across samples is roughly ±0.45 ms on a ~3.2 ms mean, so **a change
under about 15% at this viewpoint is not distinguishable from noise** with three
samples. Anything claiming a smaller win needs more samples or a quieter machine.

## Correction to the LOD-0 collapse finding

[`WTR-cdlod-lod0-collapse-20260802.md`](../decisions/WTR-cdlod-lod0-collapse-20260802.md)
recorded `lod0=24` with every other level at zero, and described the hierarchy
as never engaging. This run shows `lod0=27, lod1=1`.

**The hierarchy does engage, marginally.** The earlier reading came from a single
sample and was stated too strongly. The substance survives — the selection is
overwhelmingly level 0, and a multi-level structure delivering 27:1 is close to
single-level output — but "no node is ever selected at any coarser level" was
wrong, and the difference matters to anyone deciding whether the mechanism is
broken or merely mistuned. It is mistuned.

## How to use this

Re-measure with the same mission and sampling before and after any water change.
Treat sub-15% differences at this viewpoint as noise. Extend with an aerial and
a coastal viewpoint before drawing conclusions about water cost generally — the
aerial case in particular behaves differently, see
[`WTR-aerial-selection-latency-20260802.md`](../decisions/WTR-aerial-selection-latency-20260802.md).
