# Preview 0

Preview 0 is a **technical preview of the renderer and build**, not a game release. Its purpose is
narrow and worth stating before anything else: to show that this engine builds reproducibly, selects
the WGPU backend on purpose rather than by accident, refuses mismatched binaries instead of crashing
oddly, and produces a capture and timing bundle that someone outside the project can check.

If you are looking for new gameplay, this is not that. If you want to verify a claim this project
has made, everything below is arranged so you can.

## What this project is

> A modern open-source evolution of the Poseidon engine for large-scale combined-arms sandbox games,
> preserving OFP/CWA compatibility and progressively supporting later Real Virtuality content and
> behaviours.

That statement is directional. It describes where the work is going, not a promise that any
particular later-generation asset, mission, script, vehicle, simulation class or network behaviour
already works today. What *does* work is in the [capability matrix](capability-matrix.md), which is
generated from the project's own ledgers and re-checked by CI — not written by hand for a release.

**This is an independent project.** It is not affiliated with, endorsed by, or supported by Bohemia
Interactive. The engine source is GPL-3.0-or-later; the names and trademarks are not granted, and
the game data is not part of this project and is not distributed with it. This is emphatically not
an "open-source Arma Reforger engine": there is no Enfusion compatibility, no feature parity, no
official relationship, and no drop-in compatibility with modern Arma content.

## What you need

You must supply the original game data yourself, legally. It is not included, and it will not be.

Either works:

- A retail install of *ARMA: Cold War Assault*.
- The free *ARMA: Cold War Assault Demo* data, available on Steam at no cost.

The engine binaries are useless without one of them.

## Running it

```
ColdWarAssault.exe --render wgpu --window --dev
```

`--render` takes `wgpu`, `gl33`, or `dummy`. **GL33 remains the default**; WGPU must be asked for
explicitly. That is deliberate for a technical preview — the new backend is opt-in, and the old one
is the fallback, not a deprecated path.

Two things about the install are not obvious and cause silent, error-free failures:

1. **Both binaries must be deployed together.** `PoseidonGame.exe` and `wgpu_renderer.dll` are one
   unit. Copying only one produces an immediate `Entry Point Not Found` on launch. Mismatched
   versions are refused deliberately rather than allowed to run — see `CORE-NEG-002`.
2. **A retail install is missing files the remaster needs** — the modern Options and main-menu
   resource set, fonts, equipment icons. `scripts/Install.ps1` copies them from a Demo install.
   Skipping it produces *no error at all*: menu entries simply open nothing, because loading a
   resource class the data does not define returns a display with zero controls. If a menu appears
   dead, run the installer before assuming a code bug.

From a source checkout, the supported path is:

```powershell
.\scripts\Build.ps1     # configure + build
.\scripts\Install.ps1   # once per game install — see above
.\scripts\Start.ps1     # launch against the game data
```

## Tested configuration

Preview 0 declares **one** Tier 1 configuration rather than a hardware matrix. That is an honest
scope statement: one configuration is what can be continuously built, tested and measured, so it is
the only one whose behaviour is claimed.

| | |
| --- | --- |
| Support level | `TIER_1_RELEASE_BLOCKING` |
| OS | Windows 11 Home x64, build 26200 |
| CPU | Intel Core i7-11700F (8 cores / 16 threads) |
| RAM | 16 GB |
| GPU | NVIDIA GeForce RTX 3070 (8 GB), driver `32.0.15.9579` |
| WGPU backend | Vulkan |
| Display | 3441 x 1440 native, VSync on, no dynamic resolution |
| Frame target | 30 FPS / 33.33 ms — a release gate, not a visual mandate |

The full contract, including its gates, is in
[`docs/roadmap/tier1-preview0-validation.md`](../../roadmap/tier1-preview0-validation.md).

**No backend is pinned.** The renderer requests a high-performance adapter and takes what the
platform offers; on this configuration that is consistently Vulkan, in every recorded run. On other
machines it may be DX12. Nothing is claimed about configurations nobody has measured — including
Tier 2, of which there is none, and Linux, which builds but is not a Preview 0 gate.

## Verifying the build

Every released build carries a fingerprint, and it is the point of the release rather than a detail.
[`docs/roadmap/evidence/preview0-manifest.json`](../../roadmap/evidence/preview0-manifest.json)
records the git commit, whether the tree was dirty when it was made, the toolchain versions, the
selected adapter and backend, the negotiated capability flags, per-file SHA-256 for the runtime log,
reference capture and metrics, and a hash for every shader source that went into it.

The ledger validator refuses a manifest that records `git_dirty: true`, and re-derives the evidence
hashes rather than trusting the recorded values, so a bundle assembled from a modified tree or with
a hand-edited hash fails rather than shipping.

```sh
python scripts/validate_preview0_ledger.py
python scripts/generate_capability_matrix.py --check
```

## Known limitations

Stated plainly, because a preview that hides these is worth less than one that names them.

- **Ambient occlusion is expensive at high resolution.** GTAO costs about **32% of the GPU frame**
  in the reference mission at 3441x1440 — 7.7 ms of a 24.2 ms frame, with the horizon march alone
  the largest single item in the frame. It stays within the 33.3 ms frame budget, but it is the
  first thing to look at if this build is slower than you expect, and it scales with pixels, so a
  lower resolution helps disproportionately.
- **Interior sky visibility bakes at load, without a cache.** The per-model bake runs synchronously
  when a model is first registered, and its planned disk cache and background scheduling do not
  exist yet, so the cost is paid on every launch. Measured in the reference mission: 8 models,
  about 0.29 seconds. Missions that load many distinct models will pay proportionally more, and how
  much more has not been measured.
- **The interior lighting result has not been re-judged.** An earlier version drew criticism for
  shadow patches indoors; the cause was found and fixed, but nobody has confirmed the fixed version
  looks right in a real building. Treat it as unproven rather than accepted.
- **Texture quality has no implementation.** The setting is not wired to anything — it is absent
  from the engine entirely, not merely ineffective. Changing it does nothing.
- **GL33 is a fallback, not a parity target.** It is kept working. It is not kept equal, and visual
  differences between the two backends are expected rather than defects. Where a difference has been
  classified, the classification and its evidence are recorded rather than asserted.
- **Several renderer systems are partial.** Shadow cascades are two of four Tier 1 stages; the HDR
  pipeline is four of five, with night-vision and night-eye absent. The capability matrix marks each
  one and links its plan.
- **Every review here is owner-performed.** The project has one human. `VALIDATED` in the ledger
  means the integration owner verified it, and each ticket records what was actually exercised —
  two of them explicitly decline the smoke-test claim rather than imply a uniform check. No
  independent reviewer exists, and the record says so instead of implying otherwise.

## Fallback and removable data

Original game data is treated as read-only and is never modified. Derived caches are removable: if
you delete one, the engine falls back to computing or loading the same thing the slower way, and no
original file is touched. Nothing in this preview requires a converted or repackaged copy of your
game data in order to run.

If the WGPU backend fails to start, `--render gl33` is the supported path back. A silent fallback
from WGPU to GL33 is treated as a *failure* in the project's own tests rather than a graceful
degradation, so that "it ran" can never be mistaken for "it ran on the backend you asked for".

## Validated behaviour versus roadmap intent

The roadmap describes an ambitious long-term direction. This release claims none of it.

What Preview 0 claims is exactly the six build-truth tickets listed in the capability matrix and
nothing beyond them: reproducible builds, explicit backend selection, ABI safety, runtime
diagnostics and lifecycle handling, capture and metrics with a build fingerprint, and a platform
target definition. Any capability not marked **Works** in that matrix is not a claim, and anything
described only in the roadmap is intent.

## Not in this release

- A demonstration video. `REL-000` asks for one and it does not exist yet.
- Independent review of any kind, for the reason given above.
- Any Tier 2 or experimental hardware configuration.
- A dedicated-server validation pass; that is deferred to `TEST-004`.
